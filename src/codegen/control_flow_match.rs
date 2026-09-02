//! Match-expression codegen + pattern-condition lowering.
//!
//! Houses `compile_match` (the entry: lowers `match scrutinee { arms... }`
//! to a chain of conditional branches via `compile_pattern_condition`)
//! plus the supporting machinery:
//!
//! - `scrutinee_is_borrow_call` — receiver-borrow recognizer
//! - `compile_pattern_condition` — per-arm pattern→bool lowering
//! - `extract_enum_tag` — load the discriminant from a tagged-enum value
//! - `enum_tag_for_variant` / `variant_pattern_enum_and_tag` — variant
//!   metadata lookups (the latter qualified-path-disambiguated for the
//!   nested-variant condition recursion)
//! - `pattern_payload_word_count` / `pattern_payload_llvm_type` —
//!   per-pattern payload shape
//! - `reconstruct_payload_value` — rebuild the variant payload tuple
//!   from a pre-decomposed bit-cast
//!
//! Lives in a sibling `impl<'ctx> super::Codegen<'ctx>` block.

use crate::ast::*;
use crate::codegen::helpers::vec_inner_type_expr;

use super::state::UserDropKind;
use inkwell::basic_block::BasicBlock;
use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::{BasicValue, BasicValueEnum, FunctionValue, IntValue, PointerValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

/// A qualifying string-literal `match` selected for switch-tree dispatch
/// (the #1 real-world codegen lever — `docs/spikes/selfhost-lexer-profile.md`).
/// `arms` pairs each string-literal arm's keyword with its index into the
/// match's arm list (hence into the parallel `arm_body_bbs`); `default_arm`
/// is the index of the trailing `_` / binding catch-all if present. A String
/// `match` is exhaustive only with a catch-all, so it almost always is.
struct StringDispatchPlan {
    arms: Vec<(String, usize)>,
    default_arm: Option<usize>,
}

/// Below this many string-literal arms the linear `memcmp` cascade is already
/// cheap and its IR is simpler; only larger keyword-table-shaped matches (the
/// lexer's ~90-arm `keyword_or_ident`) are worth the switch tree.
const STRING_DISPATCH_MIN_ARMS: usize = 4;

impl<'ctx> super::Codegen<'ctx> {
    // ── Match ─────────────────────────────────────────────────────

    pub(super) fn compile_match(
        &mut self,
        scrutinee: &Expr,
        arms: &[MatchArm],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Tail-return context: consume now (the scrutinee is not a tail
        // return), re-applied per arm body below so a bare-arg `Option[shared]`
        // arm leaf gets its per-branch inc.
        let tail = self.fn_ctx.tail_ret_inner.take();
        // Keyed on the scrutinee's span, so compile ORDER is irrelevant.
        let own_value = self.branch_value_is_owned(scrutinee);
        // B-2026-08-30-2 — the block that dominates every arm and re-executes
        // on each pass through this `match`; see `arm_tail_owner_ctx`. Captured
        // before any arm is compiled, and before the cascade's own blocks
        // exist. It carries no terminator yet — the entry wiring below appends
        // one after every arm is done — so the reset store lands at its end and
        // the branch follows it.
        let match_reset_bb = self.builder.get_insert_block();
        // B-2026-08-30-2 — the `Match` arm of `branch_tail_mints_fresh_owned_temp`,
        // asked once here because an arm cannot see its siblings. True means
        // B-2026-08-29-27's widened gates free the merged value at the use
        // site, so no arm may register an owner. See `arm_tail_owner_ctx`.
        let arms_all_mint = !arms.is_empty()
            && arms
                .iter()
                .all(|a| self.branch_tail_mints_fresh_owned_temp(&a.body));
        // B-2026-09-01-10 — does the DISCARD STATEMENT site take ownership of
        // this match's value? The `compile_if` sibling has asked this since
        // B-2026-08-29-25 (`discard_stmt_owns_value`) and used it to tell each
        // arm whether its tail hands the value to anybody; `compile_match`
        // never asked, so it never set `branch_arm_value_discarded` and a
        // BLOCK-BODIED arm of a DISCARDED match compiled as though a consumer
        // existed. `compile_block_with_frame` then ran
        // `suppress_block_tail_cleanup`, which is a HANDOVER — it takes the
        // tail's buffer away from whatever owns it — with nothing on the
        // receiving end.
        //
        // The BARE-bodied spelling was already correct because
        // `compile_match`'s own arm-tail hook gates that same suppression on
        // `owns_result`; only the braced spelling reached the block compiler
        // and lost the gate. Measured at `KARAC_OPT_LEVEL=0`: `let _ = match o
        // { Some(r) => { r } None => { mk(9) } };` strands 38 B per evaluation
        // while `Some(r) => r,` on the identical value is clean.
        let discard_stmt_owns_value = !own_value && self.discarded_match_arms_qualify(arms);
        // B-2026-08-28-44 — see the `compile_if` sibling: only what THIS
        // match's arms hand out may arm the merge-point owner.
        self.branch_arm_clone_taken = None;
        // Slice 3b: when the scrutinee is a ref-typed identifier
        // (function parameter `f: ref T` / `mut ref T`), obtain the raw
        // scrutinee pointer in addition to the auto-derefed value.
        // Pattern conditions still run against the value (tag/field
        // checks are identical); leaf bindings under recognized
        // pattern shapes can then route through
        // `bind_pattern_values_via_ptr` to emit GEP-based shims that
        // alias the scrutinee storage rather than a local copy — which
        // is what makes `mut ref` write-through propagate back to the
        // caller's storage.
        let scrut_ref_ptr: Option<(PointerValue<'ctx>, StructType<'ctx>)> =
            if let ExprKind::Identifier(name) = &scrutinee.kind {
                if self.borrow_vars.ref_params.contains_key(name) {
                    let pointee = *self.borrow_vars.ref_params.get(name).unwrap();
                    if let BasicTypeEnum::StructType(st) = pointee {
                        self.get_data_ptr(name).map(|p| (p, st))
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };
        let scrut = self.compile_expr(scrutinee)?;
        // #39 — resolve the scrutinee's enum type so unqualified variant
        // patterns disambiguate against it (`Float` → `Token.Float`, not a
        // colliding `Expr.Float`). Set before any pattern-resolution call below
        // (variant→enum, variant→tag) so they prefer THIS enum over whichever
        // the unordered `enum_layouts` map happens to yield first. Gated on the
        // resolved name actually naming a known enum; a struct / unknown
        // scrutinee leaves the hint cleared so the resolvers keep their prior
        // user-vs-seed fallback. Saved/restored so a nested match doesn't leak
        // an outer scrutinee's hint inward.
        let saved_scrut_enum_hint = self.pattern_state.match_scrutinee_enum_hint.take();
        self.pattern_state.match_scrutinee_enum_hint = self
            .type_name_of_expr(scrutinee)
            .filter(|n| self.type_decls.enum_layouts.contains_key(n.as_str()));
        // `match v[i] { V(s) => … }` over a heap-element `Vec` — deep-clone the
        // shallow element so the destructure moves a payload field out of an
        // INDEPENDENT buffer, not the container's (otherwise the binding's drop
        // and `v`'s element-drop free the same buffer — double-free, the
        // direct-match sibling of B-2026-06-14-12). The clone must replace
        // `scrut` itself (not just the freshtemp alloca) so the arm's
        // `extractvalue` reads the clone; `materialize_freshtemp_enum_scrutinee`
        // then drop-tracks this same cloned value so a no-bind arm frees it.
        // No-op for every non-index / Copy-element scrutinee.
        let scrut = self.clone_owned_vec_index_element(scrutinee, scrut)?;
        // #38: a `match <self.field[i]>.enumfield { … }` scrutinee — the
        // FieldAccess-rooted-Index field shape the #18 suppression can't reach.
        // Clone the enum value so a heap payload bound out owns an independent
        // buffer (else it aliases the Vec element's buffer and dangles when the
        // container drops). The parser's `self.tokens[self.pos].token` shape.
        let (scrut, did_clone_borrowed_index_field) =
            self.clone_borrowed_index_field_enum_scrutinee(scrutinee, scrut)?;
        // B-2026-07-14-1: a bare `for`-loop element (`for p in v { match p { … } }`)
        // over a heap-bearing non-shared user ENUM whose arm MOVES a payload out.
        // The element bit-copy-aliases the container slot, so the moved payload
        // must come from an independent deep copy — the loop-element sibling of
        // the `v[i]` clone above. `did_clone` forces the clone through the
        // fresh-temp drop-tracking below (identical to the FieldAccess-index case).
        let (scrut, did_clone_loop_elem) =
            self.clone_escaping_owned_agg_loop_var_enum(scrutinee, scrut, arms);
        // One escape walk shared by both ref-chain clone legs below (their
        // cheap shape gates run first inside each leg; the walk itself only
        // runs for place-chain scrutinees).
        let ref_chain_escapes = matches!(
            scrutinee.kind,
            ExprKind::FieldAccess { .. } | ExprKind::TupleIndex { .. }
        ) && !self.no_arm_payload_escapes(arms);
        // B-2026-07-21-5/-6: an ESCAPING match over `<refparam>.field` whose
        // leaf is a heap-bearing user enum — the borrowed-chain sibling of the
        // loop-element clone above (see its doc). Same `did_clone` contract.
        let (scrut, did_clone_ref_chain) =
            self.clone_escaping_borrowed_ref_chain_enum(scrutinee, scrut, ref_chain_escapes);
        // B-2026-08-08-25 leg 3 (consuming half): the LIVE-LOCAL sibling of the
        // loop-element clone above — `match e { E.A(v) => <consume v> … }` where
        // `e` is a plain local read again after the match. Same `did_clone`
        // contract; the gates are disjoint from the loop-element leg's
        // (`for_loop_owned_agg_vars` membership) so at most one fires.
        let (scrut, did_clone_live_local_enum) =
            self.clone_escaping_live_local_enum(scrutinee, scrut, arms);
        let did_clone_borrowed_index_field = did_clone_borrowed_index_field
            || did_clone_loop_elem
            || did_clone_ref_chain
            || did_clone_live_local_enum;
        // B-2026-07-21-7: the STRUCT-leaf sibling — an ESCAPING struct-pattern
        // match over `<refparam>.field`. The clone carries its own StructDrop;
        // each arm's suppression below fires against the clone slot.
        let arm_patterns: Vec<&Pattern> = arms.iter().map(|a| &a.pattern).collect();
        let (scrut, refchain_struct_clone) = self.clone_escaping_borrowed_ref_chain_struct(
            scrutinee,
            scrut,
            &arm_patterns,
            ref_chain_escapes,
        );
        // B-2026-07-21-9: the OPTION-leaf sibling — `match <refparam>.opt {
        // Some(s) => <consume s> … }`. A consuming Some arm zeroes the clone
        // slot's tag below; None/wildcard arms leave the cleanup armed.
        let (scrut, refchain_option_clone) =
            self.clone_escaping_borrowed_ref_chain_option(scrutinee, scrut, ref_chain_escapes);
        // B-2026-08-08-25 leg 1: the LIVE-LOCAL sibling — `match o { Some(s) =>
        // <consume s> … }` where `o` is a plain local read again after the
        // match. Same clone-slot contract, so it feeds the same per-arm tag
        // zero below; the two gates are disjoint (a place chain vs a bare
        // identifier), so at most one can fire.
        let (scrut, livelocal_option_clone) =
            self.clone_escaping_live_local_option(scrutinee, scrut, arms);
        let refchain_option_clone = refchain_option_clone.or(livelocal_option_clone);
        // B-2026-07-21-10: the TUPLE-leaf sibling — `match <refparam>.pair {
        // (s, x) => <consume s> … }`. Consuming arms zero the consumed
        // elements' caps in the clone slot below.
        let (scrut, refchain_tuple_clone) =
            self.clone_escaping_borrowed_ref_chain_tuple(scrutinee, scrut, ref_chain_escapes);
        // B-2026-07-21-14: the RESULT-leaf sibling — `match <refparam>.res {
        // Ok(s) => <consume s> … }`. A consuming Ok/Err arm zeroes the clone
        // slot's payload area below; a no-bind arm leaves the cleanup armed.
        let (scrut, refchain_result_clone) =
            self.clone_escaping_borrowed_ref_chain_result(scrutinee, scrut, ref_chain_escapes);
        // B-2026-08-08-25 leg 2: the LIVE-LOCAL sibling of the line above, the
        // exact `Result` twin of the `Option` one at leg 1. Feeds the same
        // per-arm payload-area zero below, and the two gates are disjoint (a
        // place chain vs a bare identifier), so at most one can fire.
        let (scrut, livelocal_result_clone) =
            self.clone_escaping_live_local_result(scrutinee, scrut, arms);
        let refchain_result_clone = refchain_result_clone.or(livelocal_result_clone);
        // B-track (pattern-arm unbound heap-field drop): a fresh-temp enum
        // scrutinee (`match make() { … }`) has no source `EnumDrop`, so any arm
        // that leaves a heap payload field unbound leaks it. Materialize +
        // `track_enum_var` once here (enum name resolved from any variant arm —
        // all arms share the scrutinee's enum); each arm's per-arm suppression
        // below then zeroes the caps of fields THAT arm moved into bindings.
        // No-op for non-fresh / non-enum / ref scrutinees.
        //
        // `did_clone_borrowed_index_field` (#38 + [#35]): the
        // `self.toks[i].tok` FieldAccess-rooted-index shape was just deep-cloned
        // into `scrut`, so — exactly like the `clone_owned_vec_index_element`
        // heap-Vec-index case — the OWNED clone must be drop-tracked even though
        // the scrutinee Expr is a place (not a fresh-temp call). `force` makes
        // `materialize_freshtemp_enum_scrutinee` skip its scrutinee-shape gate;
        // the per-arm suppression below then zeroes the CLONE's moved-out caps
        // (line 304 path), and the container's [#35] element drain frees the
        // untouched source element. Without this the clone leaked.
        let freshtemp_enum = if scrut_ref_ptr.is_none() {
            arms.iter()
                .map(|a| &a.pattern)
                .find(|p| self.variant_pattern_enum_name(p).is_some())
                .and_then(|p| {
                    self.materialize_freshtemp_enum_scrutinee(
                        scrutinee,
                        p,
                        scrut,
                        did_clone_borrowed_index_field,
                    )
                })
        } else {
            None
        };
        // B-2026-08-30-15 — the STRUCT flavour of the same question. Mutually
        // exclusive with the enum path by construction (that one needs a
        // variant pattern, this one a user-struct pattern or a struct-returning
        // callee), but gated on it explicitly so the exclusion survives a future
        // widening of either resolver.
        let freshtemp_struct = if scrut_ref_ptr.is_none() && freshtemp_enum.is_none() {
            let pats: Vec<&Pattern> = arms.iter().map(|a| &a.pattern).collect();
            self.materialize_freshtemp_struct_scrutinee(scrutinee, &pats, scrut)
        } else {
            None
        };
        // Oversized-enum-payload §1/§2: a fresh-temp scrutinee whose payload was
        // heap-boxed (Option[Wide] / Result[Wide,_]) needs the box freed too.
        // Mutually exclusive with the user-enum path above — seeded Option /
        // Result have all-`None` drop kinds, so `materialize_freshtemp_enum_
        // scrutinee` returns None for them; the gate makes that explicit.
        let freshtemp_boxed_slot = if scrut_ref_ptr.is_none() && freshtemp_enum.is_none() {
            let pats: Vec<&Pattern> = arms.iter().map(|a| &a.pattern).collect();
            self.track_freshtemp_boxed_enum_scrutinee(scrutinee, &pats, scrut)
        } else {
            None
        };
        // Fresh-temp INLINE-heap `Result` scrutinee (`match cell.set(v) { Err(_)
        // => {} }`, B-2026-07-12-2 gap 2a): neither the enum-drop nor boxed path
        // above tracks a discarded fitting inline heap payload (a `String`/`Vec`
        // or transparent single-field wrapper Err). Register its
        // `FreeInlineResultPayload` so a no-bind arm frees it; a consuming arm's
        // per-arm `suppress_inline_result_payload_cleanup_at` (in the arm loop)
        // zeros the slot `cap`. Only for a fresh-temp, non-borrow scrutinee.
        let freshtemp_inline_res = if scrut_ref_ptr.is_none() && freshtemp_enum.is_none() {
            self.track_freshtemp_inline_result_scrutinee(scrutinee, scrut)
        } else {
            None
        };
        // Fresh-temp Option[shared] scrutinee — release the temp's
        // transferred ref (B-2026-07-15-1; see the tracker's doc).
        // B-2026-08-30-16 — the box this match must release at its OWN exit,
        // not at the enclosing scope's.
        let mut freshtemp_shared_enum: Option<PointerValue<'ctx>> = None;
        if scrut_ref_ptr.is_none() && freshtemp_enum.is_none() && freshtemp_inline_res.is_none() {
            let pats: Vec<&Pattern> = arms.iter().map(|a| &a.pattern).collect();
            self.track_freshtemp_shared_option_scrutinee(scrutinee, &pats, scrut);
            // B-2026-08-28-74 — the bare `shared` enum sibling of the line
            // above. Mutually exclusive with it by value shape: that tracker
            // needs a `StructValue` (the `Option` struct), this one the RC box
            // `PointerValue`, so at most one can fire.
            freshtemp_shared_enum =
                self.track_freshtemp_shared_enum_scrutinee(scrutinee, &pats, scrut);
        }
        // Detect borrow-returning scrutinees so pattern bindings don't
        // register a `FreeVecBuffer` against a buffer the container still
        // owns. `Map.get` is the canonical case (the returned `Option[V]`
        // aliases the bucket entry's value words); a duplicate cleanup
        // would double-free against the `karac_map_free_with_val_drop_vec`
        // path at function exit.
        let saved_borrow_flag = self.pattern_state.pattern_binding_is_borrow;
        // B-2026-08-08-25 — a read-only match over a live local that owns an
        // inline Option/Result payload is a BORROW of that payload: the source
        // keeps it, so the arm binding must not register a free and the
        // suppressors below must not disarm the source.
        let saved_source_retains = self
            .pattern_state
            .pattern_binding_source_retains_inline_payload;
        // B-2026-08-29-6 — a PASSTHROUGH CALL used directly as the scrutinee
        // (`match take(s) { .. }`, the result never bound) is the same class:
        // the callee handed back its argument, so the payload still belongs to
        // the named source, which stays armed. Without this the arm binding
        // registered its own free on top of the source's and both released the
        // same buffer — `free(): double free detected in tcache 2` on all three
        // compiled backends, for a FREE FUNCTION and a method alike, while the
        // interpreter was correct.
        //
        // Classified here rather than at a scrutinee registrar because
        // instrumentation showed none of the three claimed this scrutinee:
        // `fresh_owned_temp=true` and `passthrough=Some("s")`, yet
        // `freshtemp_enum`, `inline_res` and the shared-Option tracker all
        // declined. The second owner is the ARM BINDING, which is what this flag
        // governs.
        //
        // NARROWING the arm binding rather than zeroing the source, the
        // direction this family already settled on: B-2026-08-06-21 tried the
        // zeroing form and turned a clean program into a leak. Leaving the
        // source sole owner cannot leak — it is still armed — and the arm reads
        // the payload before the source's scope-exit free, so the borrow is live
        // for exactly as long as the arm needs it.
        //
        // The `let`-BOUND spelling of the same call is already correct through a
        // different route (B-2026-08-29-4 records a `passthrough_owner_alias` at
        // the `let` site); a scrutinee temporary never passes through that site,
        // which is why it needed its own classification here.
        let passthrough_retains = self.call_passthrough_armed_any_source(scrutinee).is_some();
        let readonly_inline_optres = {
            // B-2026-08-30-52 — the operator relaxation is scoped to this
            // classifier; see the flag's doc for the fresh-temp leak a
            // global relaxation causes.
            let saved = self.pattern_state.escape_walk_relaxes_primitive_operators;
            self.pattern_state.escape_walk_relaxes_primitive_operators = true;
            let r = self.scrutinee_is_readonly_inline_optres_local(scrutinee, arms);
            self.pattern_state.escape_walk_relaxes_primitive_operators = saved;
            r
        } || passthrough_retains;
        // B-2026-08-08-25 leg 1 — the clone leg keeps the source's payload too,
        // so the suppressors must skip it for the same reason. It is held
        // SEPARATE from `readonly_inline_optres` because only the read-only
        // classifier may also raise `pattern_binding_is_borrow` below: a cloned
        // arm binding genuinely OWNS the clone and has to register its free,
        // whereas a borrowing one registers nothing. Folding the two together
        // would drop the clone's owner and leak it.
        self.pattern_state
            .pattern_binding_source_retains_inline_payload = readonly_inline_optres
            || livelocal_option_clone.is_some()
            || livelocal_result_clone.is_some();
        // Publish the classification for the rest of the CHAIN. The flag above
        // is scoped to this match, but the combinator that consumes its result
        // (`<src>.map(f).unwrap_or(d)`) runs after the match is compiled and
        // resolves its disarm target back through the map to `<src>` — it needs
        // to know the arm left the payload where it was. B-2026-08-08-25 leg 1.
        if self
            .pattern_state
            .pattern_binding_source_retains_inline_payload
        {
            if let ExprKind::Identifier(n) = &scrutinee.kind {
                let owner = self
                    .payload_vars
                    .passthrough_owner_alias
                    .get(n.as_str())
                    .cloned()
                    .unwrap_or_else(|| n.clone());
                self.drop_rc.inline_optres_retained_sources.insert(owner);
            }
        }
        self.pattern_state.pattern_binding_is_borrow = self.scrutinee_is_borrow_call(scrutinee)
            || self.scrutinee_is_borrowed_binding(scrutinee)
            || self.scrutinee_is_readonly_borrow_mode_payload_binding(scrutinee, arms)
            || self.scrutinee_is_readonly_borrowed_place(scrutinee, arms)
            || self.scrutinee_is_readonly_owned_agg_loop_var(scrutinee, arms)
            || self.scrutinee_is_readonly_owned_enum_local(scrutinee, arms)
            || self.scrutinee_is_readonly_owned_enum_projection(scrutinee, arms)
            || readonly_inline_optres;
        // B-2026-07-15-21 Part B — scrutinee is an RC-elidable borrowed param:
        // skip the Some-binding acquire + its scope-exit RcDec (payload is a
        // proven-non-escaping alias of the caller-kept-alive param), which also
        // clears the post-call release epilogue so tailcallelim can loop-ify the
        // tail recursion.
        let saved_elidable_param_flag = self
            .pattern_state
            .pattern_binding_scrutinee_is_elidable_param;
        self.pattern_state
            .pattern_binding_scrutinee_is_elidable_param =
            self.scrutinee_is_elidable_param(scrutinee);
        // B-2026-06-13-13 residual A: when the scrutinee is the type-erased
        // `Option`/`Result`, its payload is owned by the dedicated inline/boxed
        // cleanup, not a per-field `EnumDrop` — so the pattern-binding struct
        // track must skip a bound struct payload to avoid double-freeing. Same
        // enum for every arm, so resolved once from any variant arm.
        let saved_opt_res_flag = self
            .pattern_state
            .pattern_binding_scrutinee_is_option_result;
        self.pattern_state
            .pattern_binding_scrutinee_is_option_result = arms.iter().any(|a| {
            matches!(
                self.variant_pattern_enum_name(&a.pattern).as_deref(),
                Some("Option") | Some("Result")
            )
        });
        // B-2026-07-30-11 (boxed-payload bodies): fresh owning temps
        // (`match v.pop() { … }`) are the only scrutinees whose boxed
        // payload bodies fire via the bind-site bodies-only walker — a
        // bound scrutinee's arm move already runs the body through the
        // binding-side channel.
        let saved_fresh_temp_flag = self
            .pattern_state
            .pattern_binding_scrutinee_is_fresh_owning_temp;
        self.pattern_state
            .pattern_binding_scrutinee_is_fresh_owning_temp =
            self.scrutinee_expr_is_owning_fresh_temp(scrutinee);
        // B-2026-07-10-3 — record the seed enum's inline payload-area budget
        // (Option = 3, Result = 5) so `bind_pattern_values` can tell an INLINE
        // struct payload (safe to `track_struct_var`) from a heap-BOXED one
        // (owned by the box drop). Same enum for every arm, resolved once.
        let saved_optres_area = self.pattern_state.pattern_binding_scrutinee_optres_area;
        self.pattern_state.pattern_binding_scrutinee_optres_area = arms
            .iter()
            .find_map(
                |a| match self.variant_pattern_enum_name(&a.pattern).as_deref() {
                    Some("Option") => Some(3),
                    Some("Result") => Some(5),
                    _ => None,
                },
            )
            .unwrap_or(0);
        // B-2026-06-14-31 — the scrutinee enum is a user `shared enum` (RC-boxed):
        // a struct payload bound in an arm (`Wrapped(w)`) is a by-value VIEW of
        // the box's inline payload, so its Vec/String buffer must NOT get a
        // per-binding struct value-drop (the box's rc-drop walker owns it).
        // Resolved once from any variant arm (same enum for every arm).
        let saved_shared_enum_flag = self.pattern_state.pattern_binding_scrutinee_is_shared_enum;
        self.pattern_state.pattern_binding_scrutinee_is_shared_enum = arms.iter().any(|a| {
            self.variant_pattern_enum_name(&a.pattern)
                .and_then(|n| self.type_decls.shared_types.get(&n).cloned())
                .is_some_and(|i| i.is_enum)
        });
        // B-2026-08-01-13 — scrutinee is an OWNED param: payload bindings
        // are views of the callee's entry copy; their Drop bodies belong to
        // the caller (caller-retains), so `bind_pattern_values` registers
        // memory only.
        let saved_owned_param_flag = self.pattern_state.pattern_binding_scrutinee_is_owned_param;
        self.pattern_state.pattern_binding_scrutinee_is_owned_param =
            self.scrutinee_is_owned_param_binding(scrutinee);
        // B-2026-09-02-11 — scrutinee is a heap-element INDEX read, so what the
        // arms destructure is the defensive deep clone made just below
        // (`clone_owned_vec_index_element` / the field-rooted sibling). The
        // container still owns the element and runs its payload's body at its
        // own death, so a body on the clone's binding is a second fire.
        let saved_owned_elem_clone_flag = self
            .pattern_state
            .pattern_binding_scrutinee_is_owned_elem_clone;
        self.pattern_state
            .pattern_binding_scrutinee_is_owned_elem_clone = self.expr_is_heap_vec_index(scrutinee)
            || self.expr_is_heap_vec_index_field_rooted(scrutinee);
        // B-2026-08-02-25 (match-arm leg) — the source binding's payload-bodies
        // walk is armed HERE, before any arm's suppressor runs. A consuming arm
        // retracts it; for a BOXED payload that walk is the body's only fire
        // path, so the bind site re-homes the body onto the arm binding. Sample
        // once, above the arm loop: the first consuming arm's retraction must
        // not make later arms read `None` and skip their own registration.
        //
        // B-2026-08-04-1 — the fresh-temp twin falls out of the same slot. A
        // temp has no name and so no armed walk to sample, but the boxed
        // tracker above staged the scrutinee aggregate into
        // `__freshtemp_boxed_scrut`; that slot holds the box pointer the
        // scope-exit free reads, so it is the right subject for the same
        // reason the named route uses the source's. The two are mutually
        // exclusive by construction.
        let saved_bodies_src = self
            .pattern_state
            .pattern_binding_scrutinee_payload_bodies_src;
        self.pattern_state
            .pattern_binding_scrutinee_payload_bodies_src =
            match self.scrutinee_armed_payload_bodies_action(scrutinee) {
                Some(found) => Some(found),
                None => self.freshtemp_payload_bodies_action(scrutinee, freshtemp_boxed_slot),
            };
        // B-2026-08-04-2 — the scrutinee's slot, tracked separately from the
        // bodies action above because THIS class is pure memory: a boxed
        // payload's heap fields double-free when the binding moves whether or
        // not the payload declares `impl Drop`, so the gate must not require a
        // Drop-derived walker to exist.
        let saved_optres_slot = self.pattern_state.pattern_binding_scrutinee_optres_slot;
        self.pattern_state.pattern_binding_scrutinee_optres_slot =
            self.scrutinee_optres_slot(scrutinee, freshtemp_boxed_slot);
        let fn_val = self.current_fn.unwrap();
        let merge_bb = self.context.append_basic_block(fn_val, "match.merge");

        let mut arm_results: Vec<(BasicValueEnum<'ctx>, BasicBlock<'ctx>)> = Vec::new();
        // B-2026-08-30-11 — one entry per arm that reaches the merge:
        // `(what it reported, its value, the block its store belongs in,
        // whether its tail MINTS)`. Registration is deferred until every arm
        // has been compiled, because a minting arm's owner has to come from a
        // SIBLING's frame and no arm knows its siblings' answers while it is
        // being compiled. See `Codegen::mixed_branch_arm_owner`.
        #[allow(clippy::type_complexity)]
        let mut arm_owner_records: Vec<(
            Option<(Option<BasicTypeEnum<'ctx>>, Option<usize>)>,
            BasicValueEnum<'ctx>,
            BasicBlock<'ctx>,
            bool,
            bool,
        )> = Vec::new();
        // B-2026-08-30-49 — signedness of each arm's tail, captured HERE
        // because `arm.body` is in scope at the push site and gone by the
        // merge. Parallel to `arm_results` and pushed with it (one push site).
        let mut arm_unsigned: Vec<bool> = Vec::new();

        // String-literal dispatch (#1 codegen lever — selfhost-lexer-profile.md):
        // a `match s { "kw" => .., …, _ => .. }` over ≥4 string literals
        // otherwise lowers to a linear `memcmp` cascade (one length-check +
        // `memcmp` per arm — `keyword_or_ident`'s ~90 arms were 46% of the
        // self-hosted lexer's self-time). When the arms qualify we keep every
        // arm BODY block exactly as the cascade builds it (all the binding /
        // drop / tail-move machinery below is untouched) and only replace the
        // ENTRY path: a `len` switch → first-byte switch → residual `memcmp`
        // tree that branches straight into the matching body. Skipping the
        // per-arm condition is sound — a string-literal pattern binds nothing
        // and has no side effects, and the default arm binds inside its body.
        let dispatch_plan = self.analyze_string_dispatch(arms);
        let entry_bb = self.builder.get_insert_block().unwrap();

        let arm0_bb = self.context.append_basic_block(fn_val, "match.arm0");
        // Entry branch is DEFERRED to after the loop: when `dispatch_plan` is
        // Some we branch `entry_bb` through the switch tree instead of into the
        // cascade. Collect each arm's body block as the loop builds it so the
        // tree can target them.
        let mut next_bb = arm0_bb;
        let mut arm_body_bbs: Vec<BasicBlock<'ctx>> = Vec::with_capacity(arms.len());

        // B-2026-08-31-14 — `borrowed_agg_payload_struct_vars` is keyed by
        // BINDING NAME and cleared only per FUNCTION, so a name registered by a
        // borrow-mode arm stayed registered for every LATER match in the same
        // function that happened to reuse it. Snapshot it around the arm loop
        // and restore per arm, so the registration lives exactly as long as the
        // arm that made it.
        let saved_borrowed_agg_payload_vars =
            self.borrow_vars.borrowed_agg_payload_struct_vars.clone();
        // B-2026-08-31-23 — same per-arm scoping, same reason (B-2026-08-31-14):
        // the map is keyed by BINDING NAME, so an alias recorded by one arm
        // must not be inherited by a later one that reuses the name.
        let saved_boxed_payload_alias = self.payload_vars.boxed_payload_alias.clone();
        for (i, arm) in arms.iter().enumerate() {
            self.borrow_vars.borrowed_agg_payload_struct_vars =
                saved_borrowed_agg_payload_vars.clone();
            self.payload_vars.boxed_payload_alias = saved_boxed_payload_alias.clone();
            let arm_bb = next_bb;
            // Always create a fresh fail_bb — never reuse merge_bb directly.
            // If the last pattern condition is false (non-exhaustive match or
            // missed case), we emit `unreachable` to satisfy LLVM's requirement
            // that every basic block has a terminator and every phi predecessor
            // is accounted for.
            let is_last = i + 1 == arms.len();
            let fail_bb = if !is_last {
                self.context
                    .append_basic_block(fn_val, &format!("match.arm{}", i + 1))
            } else {
                self.context.append_basic_block(fn_val, "match.nofall")
            };
            next_bb = fail_bb;

            self.builder.position_at_end(arm_bb);

            // Slice arms route through the SliceSource-driven helper —
            // the generic `compile_pattern_condition` Slice fall-through
            // would always-match and clobber length dispatch.
            let cond = if let PatternKind::Slice {
                prefix,
                rest,
                suffix,
            } = &arm.pattern.kind
            {
                let src = self.resolve_slice_source(scrutinee).ok_or_else(|| {
                    "slice pattern requires an identifier scrutinee resolvable to Array/Vec/Slice"
                        .to_string()
                })?;
                self.compile_slice_pattern_condition(prefix, rest, suffix, &src)?
            } else {
                self.compile_pattern_condition(&arm.pattern, scrut)?
            };

            let body_bb = self
                .context
                .append_basic_block(fn_val, &format!("match.body{}", i));
            arm_body_bbs.push(body_bb);

            self.builder
                .build_conditional_branch(cond.into_int_value(), body_bb, fail_bb)
                .unwrap();

            self.builder.position_at_end(body_bb);

            // Per-arm scope frame: cleanups registered during this arm's
            // pattern binding + body compilation fire at end-of-arm rather
            // than end-of-function. Closes the 2026-05-13 alloca-reuse leak
            // for loop-driven match arms (e.g. `while ... { match bucket
            // .remove(k) { Some(indices) => ... } }` — `indices`'s alloca
            // is hoisted to entry and reused N times, but only the last
            // value's cleanup fired at fn-end; the other N-1 leaked).
            // Frame is popped either by `drain_top_frame_with_emit` (the
            // fall-through-to-merge path below) or `scope_cleanup_actions
            // .pop()` (the early-return path, where the return's own
            // `emit_scope_cleanup` already walked the full stack including
            // this frame and emitted cleanup for its actions).
            self.drop_rc.scope_cleanup_actions.push(Vec::new());

            // B-2026-07-13-6: an arm's PATTERN bindings (`Some(v)`) and body
            // `let`s are ARM-scoped. Checkpoint the name env here so they revert
            // at end-of-arm — otherwise a payload/body binding that SHADOWS an
            // outer name leaks to a sibling arm that references the outer name,
            // and to the code after the `match`. Reverted after this arm's
            // cleanup frame drains (below); the arm VALUE is an already-captured
            // SSA value, unaffected by the revert.
            let arm_snap = self.snapshot_var_env();

            // Bind pattern variables
            if let PatternKind::Slice {
                prefix,
                rest,
                suffix,
            } = &arm.pattern.kind
            {
                let src = self.resolve_slice_source(scrutinee).ok_or_else(|| {
                    "slice pattern requires an identifier scrutinee resolvable to Array/Vec/Slice"
                        .to_string()
                })?;
                self.bind_slice_pattern(prefix, rest, suffix, &src, true)?;
            } else {
                // Slice 3b: try the pointer-source binding path first
                // when we have a ref-scrutinee. If the pattern shape
                // isn't recognized by `bind_pattern_values_via_ptr`
                // (e.g., or-patterns, at-bindings, slice patterns,
                // multi-word payloads), fall back to slice 3a's
                // value-source + ref-shim path which still produces
                // correct (though copy-aliased) bindings.
                // B-2026-07-30-11 (match-arm leg): record which of this arm's
                // binding names sit in a VARIANT payload position, so
                // `bind_pattern_values` can route a Drop-declaring payload
                // struct to the UserDrop channel. Name-scoped to this bind
                // call and cleared right after — bare-tuple elements never
                // enter the set, keeping the tuple-element walk the single
                // body owner for those.
                self.pattern_state.current_variant_payload_bindings.clear();
                {
                    let mut vp_names: Vec<String> = Vec::new();
                    Self::collect_variant_payload_binding_names(&arm.pattern, false, &mut vp_names);
                    self.pattern_state
                        .current_variant_payload_bindings
                        .extend(vp_names);
                }
                // B-2026-08-31-7 — and the complement, for VIEW-NESS rather
                // than for the arm channel. A bare-tuple element bound out of
                // an owned-param scrutinee (`match t { (r, k) => … }`, and the
                // projected `match s.t { … }`) is a view of the callee's entry
                // copy on exactly the terms the payload above is; the element
                // walk stays the single body owner, and a REBIND of the
                // element must inherit that rather than mint a second one.
                // See `collect_bare_tuple_binding_names` for the measurements.
                // Deliberately kept OUT of `current_variant_payload_bindings`:
                // that set routes to the UserDrop channel, which is the one
                // thing a tuple element must not do.
                if self.pattern_state.pattern_binding_scrutinee_is_owned_param {
                    let mut bt_names: Vec<String> = Vec::new();
                    Self::collect_bare_tuple_binding_names(&arm.pattern, false, &mut bt_names);
                    self.payload_vars.param_view_locals.extend(bt_names);
                }
                // B-2026-09-02-23 — and the MEMORY half, which is a different
                // question from view-ness and is NOT restricted to an
                // owned-param scrutinee. A bare-tuple element binding is a
                // bit-copy of the tuple's element, and the tuple's own
                // `__karac_drop_tuple_*` (emitted at the merge block) already
                // frees that element's heap; registering the binding on the
                // struct-drop channel too freed the same buffers twice.
                // `-O2` hid almost every instance -- which is why this sat
                // behind a `Vec`+`String` "trigger" that was really just the
                // one shape the optimizer failed to fold -- and
                // `KARAC_OPT_LEVEL=0` shows it on ALL of them, including a
                // `match t { (r, k) => { println("hi") } }` that reads no
                // field at all.
                self.pattern_state.current_bare_tuple_bindings.clear();
                {
                    let mut bt_all: Vec<String> = Vec::new();
                    Self::collect_bare_tuple_binding_names(&arm.pattern, false, &mut bt_all);
                    self.pattern_state
                        .current_bare_tuple_bindings
                        .extend(bt_all);
                }
                // B-2026-08-12-2 — per-ARM, and saved/restored because a nested
                // `match` inside this body binds through the same field.
                let saved_arm_borrows = self.pattern_state.pattern_binding_arm_only_borrows;
                self.pattern_state.pattern_binding_arm_only_borrows = self
                    .arm_only_borrows_inline_result_payload(
                        &arm.pattern,
                        &arm.body,
                        arm.guard.as_ref(),
                    );
                // B-2026-08-28-63 — the payload bindings this arm merely READS.
                // A binding the arm CONSUMES (returns as the arm's value,
                // passes to a sink, rebinds) has handed its `Drop` body to
                // whoever took it, so registering one here as well runs the
                // body twice; measured `dE dE kept` for
                // `let k = match o { Some(e) => { id(e) } .. }` and
                // `dR1 dR1 kept` for the struct spelling, against one in the
                // interpreter. Same classifier the flag above is built from,
                // applied per BINDING rather than per arm because a
                // multi-binding pattern can borrow one payload and move
                // another. Saved/restored: a nested `match` inside this body
                // rebinds through the same field.
                let saved_arm_borrowed_names =
                    std::mem::take(&mut self.pattern_state.pattern_binding_arm_borrowed_only_names);
                {
                    let mut names: Vec<String> = Vec::new();
                    Self::collect_variant_payload_binding_names(&arm.pattern, false, &mut names);
                    self.pattern_state.pattern_binding_arm_borrowed_only_names = names
                        .into_iter()
                        .filter(|n| {
                            super::consume_class::binding_only_borrowed(n, &arm.body)
                                && arm.guard.as_ref().is_none_or(|g| {
                                    super::consume_class::binding_only_borrowed(n, g)
                                })
                        })
                        .collect();
                }
                let handled_via_ptr = if let Some((scrut_ptr, pointee_ty)) = scrut_ref_ptr {
                    self.bind_pattern_values_via_ptr(&arm.pattern, scrut_ptr, pointee_ty)?
                        .is_some()
                } else {
                    false
                };
                if handled_via_ptr {
                    self.pattern_state.pattern_binding_arm_only_borrows = saved_arm_borrows;
                    self.pattern_state.pattern_binding_arm_borrowed_only_names =
                        saved_arm_borrowed_names.clone();
                }
                if !handled_via_ptr {
                    self.bind_pattern_values(&arm.pattern, scrut)?;
                    self.pattern_state.pattern_binding_arm_only_borrows = saved_arm_borrows;
                    self.pattern_state.pattern_binding_arm_borrowed_only_names =
                        saved_arm_borrowed_names.clone();
                    self.pattern_state.current_variant_payload_bindings.clear();
                    self.pattern_state.current_bare_tuple_bindings.clear();
                    // B-2026-09-02-27 — record where each bare-tuple element
                    // binding's value REALLY lives, now that the binding's own
                    // alloca exists. The arm binds a bit-copy of the element;
                    // the source is the tuple's own storage, and a later
                    // per-field move-out has to neutralize BOTH.
                    self.record_bare_tuple_elem_sources(&arm.pattern, scrutinee);
                    // Slice 3s (B-2026-07-01-12): a borrow-mode `Some(x)` bind
                    // over a `Map.get` scrutinee whose arm (guard or body)
                    // MOVES `x` gets a deep clone + owned tracking — the
                    // escaping value must not alias the bucket's storage.
                    if self.pattern_state.pattern_binding_is_borrow {
                        let mut scope: Vec<&Expr> = vec![&arm.body];
                        if let Some(g) = &arm.guard {
                            scope.push(g);
                        }
                        self.clone_escaping_borrow_payload_binding(
                            scrutinee,
                            &arm.pattern,
                            Some(&scope),
                            &[],
                        )?;
                    }
                }
                // B-2026-07-17-20: on the borrow path, a struct payload binding
                // aliases the container-owned enum element, so a Vec/String field
                // COPIED OUT of it (`let ps = f.params`) must deep-copy. Register
                // the struct payload binding name(s) so
                // `deep_copy_owned_struct_param_field_move` fires (the enum-payload
                // sibling of the for-loop struct-element path). Covers both the
                // ptr-bind and value-bind routes.
                if self.pattern_state.pattern_binding_is_borrow {
                    self.register_borrowed_agg_payload_struct_bindings(&arm.pattern);
                } else {
                    // B-2026-08-31-23 — on the TRANSFER path, a whole-payload
                    // binding over a boxed payload is a bit-copy ALIAS of the
                    // box interior. Record what it aliases so a nested match
                    // that moves a field out of it can disarm the box, not
                    // just the copy. Borrow-path arms are excluded: there the
                    // source keeps the payload and nothing needs disarming.
                    self.register_boxed_payload_alias(scrutinee, &arm.pattern);
                }
            }

            // Arm GUARD (`pat if cond => body`): after the pattern matched and
            // its bindings are in scope, evaluate the guard expression. When it
            // is false, Rust semantics fall through to the NEXT arm's pattern
            // test — so branch to `fail_bb`. This must run BEFORE the move/
            // suppression machinery below and before the body: on the guard-
            // false edge nothing is moved out (the body never runs) and we emit
            // NO binding cleanup, so the scrutinee source retains ownership and
            // frees each heap payload exactly once (the bindings are aliases
            // into the still-owned source). The per-arm scope frame pushed above
            // is left intact for the guard-pass path, where the body appends to
            // it and the end-of-arm drain fires it once (with the source caps
            // zeroed by the suppression below → exactly one free). Guards were
            // previously never emitted, so the arm fired whenever its pattern
            // matched regardless of the condition (B-2026-07-12-9).
            if let Some(guard) = &arm.guard {
                let guard_val = self.compile_expr(guard)?.into_int_value();
                let guard_pass_bb = self
                    .context
                    .append_basic_block(fn_val, &format!("match.guardpass{}", i));
                self.builder
                    .build_conditional_branch(guard_val, guard_pass_bb, fail_bb)
                    .unwrap();
                self.builder.position_at_end(guard_pass_bb);
            }

            // Value-move destructure: when the scrutinee is an owned
            // enum binding and the arm's pattern is `Variant(...args)`
            // binding heap-bearing payload fields (Vec / String), the
            // destructure has moved ownership into the new bindings —
            // the per-arm cleanup will free those buffers when the
            // bindings drop. The source enum's `__karac_drop_<E>` call
            // (queued by `track_enum_var` at the binding's let-site
            // and fires at the *outer* scope's drain) would read the
            // source's still-populated payload words and re-free the
            // same buffers → double-free. Zero the source's `cap`
            // word(s) for each consumed heap-bearing field so the
            // drop-switch's `cap > 0` guard skips. Mirrors the Vec /
            // String / shared-struct suppression in
            // `suppress_source_vec_cleanup_for_arg`. Ref-scrutinee
            // matches don't need this — the source isn't owned by the
            // match, no double-free risk.
            if scrut_ref_ptr.is_none() && !self.pattern_state.pattern_binding_is_borrow {
                if let Some((alloca, enum_name)) = &freshtemp_enum {
                    // Fresh-temp scrutinee: suppress against the materialized
                    // alloca (no identifier to resolve). The source EnumDrop
                    // registered before the arm loop frees this arm's unbound
                    // heap fields at scope exit.
                    self.suppress_destructured_enum_payload_cleanup_at(
                        *alloca,
                        enum_name,
                        &arm.pattern,
                    );
                } else if let Some((alloca, struct_name)) = &freshtemp_struct {
                    // B-2026-08-30-15 — the struct twin of the line above. An
                    // arm that MOVES a field out must zero that field's cap in
                    // the materialized slot, or the wrapper's field cleanup and
                    // the arm binding both free it. (The field's BODY still runs
                    // on the husk — B-2026-08-31-31, which the bound spelling
                    // has too and which this deliberately does not try to fix
                    // from here.)
                    self.suppress_destructured_struct_pattern_cleanup_at(
                        *alloca,
                        struct_name,
                        &arm.pattern,
                    );
                } else {
                    self.suppress_destructured_enum_payload_cleanup(scrutinee, &arm.pattern);
                    // B-2026-08-07-7 — the BOXED-payload struct-field channel
                    // the call above cannot reach (`is_heap_bearing()` is false
                    // for `BoxedOptRes`, so it skips boxed payloads by design).
                    self.suppress_struct_field_boxed_payload_match_out(
                        scrutinee,
                        &arm.pattern,
                        &arm.body,
                    );
                    // B-2026-08-12-15 — the mirror image of the call above: not
                    // a struct FIELD scrutinee whose box the struct's drop
                    // owns, but a whole enum scrutinee whose inline struct
                    // payload the ARM's drop takes over.
                    self.suppress_struct_field_boxed_payload_arm_bind(scrutinee, &arm.pattern);
                    // B-2026-08-28-66 — a WHOLE boxed payload bound out by a
                    // consuming arm (`Some(r) => { r }`). The line above covers
                    // a boxed payload reached through a struct FIELD; this one
                    // covers the plain binding, whose fields were freed by the
                    // box's inner walk as well as by whoever received the value.
                    self.suppress_boxed_payload_whole_binding(scrutinee, &arm.pattern, &arm.body);
                    // B-2026-09-01-30 — a whole-value arm binding over an
                    // OWNED by-value param inherits that param's ownership.
                    //
                    // `owned_vecstr_params` is what makes a retaining consume
                    // site deep-copy instead of moving, and it holds PARAM
                    // NAMES. `match b { v => { return v; } }` returns `v`, not
                    // `b`, so the return-position copy asked about a name that
                    // was not in the set, moved the caller's buffer out, and
                    // the caller freed it a second time. Measured on
                    // `takeout[T](b: T) -> T` at `Vec[String]`: three invalid
                    // frees, both element buffers and the outer one.
                    //
                    // The bare `return b;` spelling of the same function is
                    // correct, which is the non-uniformity the row is about:
                    // one convention, two answers, depending on whether the
                    // value reached the return through a pattern binding.
                    self.propagate_owned_param_to_whole_arm_binding(scrutinee, &arm.pattern);
                    // B-2026-06-10-6 companion: the erased-`Option` drop
                    // switch can't classify an inline `String`/`Vec` payload,
                    // so the suppression above no-ops for it. Zero the source
                    // `Option`'s `cap` here when an arm binds the `Some`
                    // payload out — its `FreeInlineOptionPayload` scope-exit
                    // free would otherwise double-free against the binding.
                    self.suppress_inline_option_payload_cleanup(scrutinee, &arm.pattern);
                    // B-2026-08-05-3: skip disarming the source when the arm
                    // only BORROWS a whole-TUPLE payload — that binding has no
                    // owner of its own, so the source must free it. Mirrors the
                    // `Option` gate below (B-2026-07-03-31); no-ops for every
                    // non-tuple payload.
                    if !self.arm_only_borrows_result_tuple_payload(
                        scrutinee,
                        &arm.pattern,
                        &arm.body,
                        arm.guard.as_ref(),
                    ) {
                        self.suppress_inline_result_payload_cleanup(scrutinee, &arm.pattern);
                    }
                    // B-2026-08-05-3 (Option leg): a consuming arm takes the
                    // BOXED tuple payload's interior, so downgrade the box drop
                    // to box-only. No-op for every non-tuple payload.
                    self.retract_boxed_tuple_inner_drop_for_arm(
                        scrutinee,
                        &arm.pattern,
                        &arm.body,
                        arm.guard.as_ref(),
                    );
                    // B-2026-08-03-3 — the same suppression for a TUPLE-ELEMENT
                    // scrutinee (`match t.0 { Result.Err(e) => .. }`). The two
                    // above key on a BINDING in `inline_{option,result}_payload_
                    // vars`; `t.0` has no binding of its own, and the tuple's
                    // element drop only started freeing that payload in this
                    // slice — so without this the consuming arm and the tuple
                    // drop both freed it.
                    self.suppress_tuple_elem_optres_payload_cleanup(scrutinee, &arm.pattern);
                    // B-2026-07-30-11 (Option/Result leg): bodies retraction
                    // beside the memory suppressions — see the fn's doc.
                    self.suppress_optres_payload_bodies_for_match(scrutinee, &arm.pattern);
                    // Fresh-temp inline `Result` scrutinee (B-2026-07-12-2 gap
                    // 2): suppress the source's payload free on a CONSUMING arm so
                    // the binding / consumer owns the buffer — UNLESS the arm only
                    // BORROWS a STRUCT-WRAPPER payload (`Err(e) =>
                    // e.rejected.len()`). A struct-wrapper binding
                    // (`AlreadySetError[String]`) has NO cleanup of its own (a
                    // struct payload is not `track_vec_var`'d), so on a recover-
                    // READ the source must free it at arm-end, else it leaks. A
                    // DIRECT `String`/`Vec` payload binding DOES get its own
                    // `track_vec_var` free, so it must ALWAYS suppress here (else
                    // double-free) — hence the wrapper gate.
                    if let Some(slot) = freshtemp_inline_res {
                        let borrow_only = self.arm_only_borrows_inline_result_payload(
                            &arm.pattern,
                            &arm.body,
                            arm.guard.as_ref(),
                        );
                        let wrapper =
                            self.inline_result_payload_binding_is_struct_wrapper(&arm.pattern);
                        // B-2026-07-23-4: a heap FIELD of the wrapper binding
                        // passed BY VALUE to a free fn (`println(w.s)`) is
                        // MOVED — the callee frees it. `consume_class` scores a
                        // free-fn arg as non-consuming, so `borrow_only` is
                        // spuriously true for these; without suppressing here the
                        // still-armed source payload drop double-frees that same
                        // buffer. Force suppression whenever the arm moves a heap
                        // field into a free-fn arg, overriding the borrow-only
                        // wrapper skip (which exists only for TRUE reads like
                        // `Err(e) => e.rejected.len()`).
                        let heap_field_moved_to_free_fn = self
                            .wrapper_arm_moves_heap_field_to_free_fn(
                                &arm.pattern,
                                &arm.body,
                                arm.guard.as_ref(),
                            );
                        // B-2026-08-04-11 leg (b): the wrapper skip's premise —
                        // "a struct-wrapper binding registers no cleanup of its
                        // own" — was falsified by B-2026-07-10-3, which started
                        // tracking an INLINE struct payload so its inner heap
                        // fields get freed. When the bind site took ownership
                        // the source must ALWAYS suppress, borrow-only or not,
                        // else the overlay free and the binding's
                        // `__karac_drop_<S>` both fire on the same buffer.
                        let binding_owns_payload =
                            self.inline_result_payload_binding_registers_own_drop(&arm.pattern);
                        // B-2026-08-11-29 — the WHOLE binding moved by value
                        // into a free fn, the sibling of the field case above.
                        // The callee owns a by-value struct param and drops it,
                        // so the source payload must be suppressed exactly as it
                        // is when a field is moved.
                        let whole_binding_moved_to_free_fn = self
                            .wrapper_arm_moves_whole_binding_to_free_fn(
                                &arm.pattern,
                                &arm.body,
                                arm.guard.as_ref(),
                            );
                        if !(borrow_only && wrapper)
                            || heap_field_moved_to_free_fn
                            || whole_binding_moved_to_free_fn
                            || binding_owns_payload
                        {
                            self.suppress_inline_result_payload_cleanup_at(slot, &arm.pattern);
                        }
                    }
                    self.suppress_inline_option_map_payload_cleanup(scrutinee, &arm.pattern);
                    // B-2026-07-03-31: skip disarming the source payload drop
                    // when the arm ONLY BORROWS the bound payload (it is not
                    // moved out) — the source must free it, else it leaks.
                    if !self.arm_only_borrows_option_agg_payload(
                        scrutinee,
                        &arm.pattern,
                        &arm.body,
                        arm.guard.as_ref(),
                    ) {
                        self.suppress_inline_option_agg_payload_cleanup(scrutinee, &arm.pattern);
                    }
                    // B-2026-08-29-2 — retract this binding's LEAF interior
                    // drop when the arm binds the value that drop frees.
                    // Outside the borrow-only gate above on purpose: the
                    // question here is ownership of the leaf, not whether the
                    // source keeps its payload, and the depth test is what
                    // makes it precise.
                    self.retract_boxed_leaf_drop_for_consuming_pattern(scrutinee, &arm.pattern);
                    // Slice 3t: struct-destructure of a BOXED payload — zero
                    // the consumed fields inside the box so the binding's
                    // BoxedEnumDrop inner walk frees only unbound fields.
                    self.suppress_boxed_payload_struct_destructure(scrutinee, &arm.pattern);
                    // B-2026-08-04-6 — the FRESH-TEMP twin: same per-field
                    // split, against the box staged by
                    // `track_freshtemp_boxed_enum_scrutinee`. No-ops for a
                    // named or borrow scrutinee (the slot is `None`).
                    self.suppress_freshtemp_boxed_payload_struct_destructure(
                        freshtemp_boxed_slot,
                        &arm.pattern,
                    );
                }
                // #15: a struct-FIELD enum scrutinee (`match spanned.tok { … }`).
                // Runs regardless of the identifier/fresh-temp split above —
                // both neutralize only the scrutinee copy, never the enum field
                // in the SOURCE struct, which the owning struct's drop now frees.
                self.suppress_destructured_struct_field_enum_cleanup(scrutinee, &arm.pattern);
                // B-2026-07-21-16: the seeded Option/Result sibling of #15 —
                // `match a.opt { Some(s) => … }` over an OWNED place. The #15
                // route hands these to the generic enum suppressor, which
                // no-ops on the seeded all-None drop-kind layouts, so the
                // source field stayed armed and the struct drop double-freed
                // the consumed payload. Zero the source in the binding arm.
                let optres_bindings_owned = !self.pattern_state.pattern_binding_is_borrow;
                self.suppress_consumed_place_optres_field_source(
                    scrutinee,
                    &arm.pattern,
                    optres_bindings_owned,
                );
                // B-2026-07-22-2: the fresh-temp sibling — `match mk().opt {
                // Some(s) => … }` zeroes the accessed field in the staged
                // temp slot on a consuming arm.
                self.consume_freshtemp_field_scrutinee(
                    scrutinee,
                    &arm.pattern,
                    optres_bindings_owned,
                );
                // #16: a plain struct-pattern destructure (`match v { S { s } =>
                // … }`) of an owned local struct. Cap-zero each consumed field in
                // the source slot so the scrutinee's struct drop skips the buffer
                // the new binding now owns.
                self.suppress_destructured_struct_pattern_cleanup(scrutinee, &arm.pattern);
                // B-2026-08-31-26 — the BODIES half of #16. The line above
                // zeroes the moved-out fields' memory in the source; without
                // this the source's bodies walk still visited them and ran a
                // user `Drop` body a second time on the cleared husk.
                self.disarm_arm_destructured_struct_field_bodies(scrutinee, &arm.pattern);
                // B-2026-07-21-7: ref-chain struct clone — the expr-based
                // suppression above bails on the borrowed root, so fire the
                // same per-field cap-zeroing against the CLONE slot instead
                // (its StructDrop then frees only the fields this arm left
                // unbound).
                if let Some((clone_ptr, clone_name)) = &refchain_struct_clone {
                    let (clone_ptr, clone_name) = (*clone_ptr, clone_name.clone());
                    self.suppress_destructured_struct_pattern_cleanup_at(
                        clone_ptr,
                        &clone_name,
                        &arm.pattern,
                    );
                }
                // B-2026-07-21-9: ref-chain Option clone — a consuming Some
                // arm zeroes the clone's tag so its FreeInlineOptionPayload
                // skips the payload the binding now owns.
                if let Some(clone_slot) = refchain_option_clone {
                    self.zero_refchain_option_clone_on_consume(clone_slot, &arm.pattern);
                }
                // B-2026-07-21-10: ref-chain tuple clone — zero the consumed
                // elements' caps in the clone slot.
                if let Some((slot, agg_ty, ref elem_tes)) = refchain_tuple_clone {
                    let elem_tes = elem_tes.clone();
                    self.zero_refchain_tuple_clone_on_consume(
                        slot,
                        agg_ty,
                        &elem_tes,
                        &arm.pattern,
                    );
                }
                // B-2026-07-21-14: ref-chain Result clone — a consuming
                // Ok/Err arm zeroes the clone's payload area so its
                // FreeInlineResultPayload skips the buffer the binding now
                // owns.
                if let Some(slot) = refchain_result_clone {
                    self.suppress_inline_result_payload_cleanup_at(slot, &arm.pattern);
                }
                // Shared-enum analog: a `match e { S(s) => s }` over a SHARED
                // enum box (`scrut` = the RC box pointer) that MOVES a Vec/String
                // payload out into a binding must zero that field's `cap` word IN
                // THE BOX, so the box's `__karac_rc_drop_<E>` (whose Vec/String
                // arm frees `cap > 0`) skips the buffer the binding now owns —
                // else the binding (or the value it's moved into, e.g. a returned
                // String) frees it AND the box's eventual rc-drop frees it again
                // (double-free; the untested `shared enum E { S(String) }` move-out
                // shape). The other suppressors above bail on shared enums; this
                // is their missing shared sibling. `scrut` is the box pointer for
                // a shared scrutinee — a no-op pointer-shape / non-shared self-bail.
                if let Some(en) = self.variant_pattern_enum_name(&arm.pattern) {
                    if scrut.is_pointer_value() {
                        self.suppress_shared_enum_payload_move_out(
                            scrut.into_pointer_value(),
                            &en,
                            &arm.pattern,
                        );
                    }
                }
            }

            // B-2026-08-30-2 — hand the arm context down only when the body is
            // DIRECTLY a block, which is the only shape that reaches
            // `compile_block_with_frame` as this arm rather than as something
            // nested inside it. Every other body shape is handled by this
            // function's own hook after the drain below; setting the one-shot
            // for those would let a value-position block buried in the body
            // consume it and file its owner under the match's key.
            let body_is_block = matches!(
                &arm.body.kind,
                ExprKind::Block(_)
                    | ExprKind::Seq(_)
                    | ExprKind::Unsafe(_)
                    | ExprKind::LabeledBlock { .. }
            );
            if own_value && body_is_block {
                self.arm_tail_owner_ctx = match (self.current_branch_expr_span, match_reset_bb) {
                    (Some(span), Some(bb)) => Some((span, arms_all_mint, bb)),
                    _ => None,
                };
            }
            // B-2026-09-01-10 — the `compile_if` signal, on the same
            // directly-a-block gate the arm context above uses: only a body
            // that reaches `compile_block_with_frame` AS THIS ARM may consume
            // it, and a value-position block buried inside some other body
            // shape must get the ordinary treatment. Bare-bodied arms are
            // covered by this function's own `owns_result` gate after the
            // drain, so setting it for them would double the answer.
            self.branch_arm_value_discarded =
                body_is_block && !own_value && !discard_stmt_owns_value;
            let mut arm_val = self.compile_tail_final_expr(&arm.body, tail)?;
            self.arm_tail_owner_ctx = None;
            // B-2026-09-01-10 — one-shot, like the context above: the block
            // compiler `take`s it, and an arm whose body never reached that
            // compiler must not leave it armed for the next construct.
            self.branch_arm_value_discarded = false;
            // B-2026-08-30-2 — a BLOCK arm body reports through here; a bare one
            // is picked up from `vecstr_source_disarmed` below. Either way the
            // registration happens after this arm's OWN frame drains, which is
            // the whole reason the block cannot do it itself.
            let arm_pending = self.arm_pending_tail_owner.take();
            let arm_body_end = self.builder.get_insert_block().unwrap();
            if arm_body_end.get_terminator().is_none() {
                // Move-aware: if the arm's tail expression is an
                // Identifier for a tracked Vec / String, the value is
                // being moved into the match's result (caller now owns
                // the buffer). Zero the source's `cap` so the per-arm
                // cleanup's `cap > 0` guard skips, preventing double-free
                // (analogous to `suppress_cleanup_for_tail_return` for
                // function-level Vec returns). Identifier match-arm
                // tail-return is the canonical Option-unwrap shape
                // `match opt { Some(v) => v, None => default() }`.
                // B-2026-08-28-51 — the conditional-move flag for a bare
                // identifier arm tail, the `match` sibling of the hook in
                // `compile_block_with_frame`. `match n { 0 => a, 1 => b, _ =>
                // R { id: 9 } }` in escaping position moves whichever local the
                // TAKEN arm names and leaves the others to die in place, which
                // is the same missing runtime bit shape A needs. Emitted here,
                // in the arm's own basic block, so the clear is path-local.
                self.arm_conditional_move_tail_flag(&arm.body);
                // B-2026-08-29-13 — the bare-`shared` TRANSFER inc is gated on
                // the match's result actually having an owner, the same
                // `branch_value_is_owned` reasoning the boxed-payload
                // neutralizer below records: "neutralizing assumes the
                // destination registers a drop of its own, and a DISCARDED
                // match result has no destination at all". The transfer's inc
                // is the consumer's ref; with no consumer it is never spent and
                // the box leaks (`match t { Bin(l, r) => l };` in statement
                // position, 11 allocs / 10 frees). Only the shared arm is gated
                // — every other suppression this call performs still runs.
                let owns_result = self.branch_value_is_owned(scrutinee);
                // B-2026-08-29-5 — and the WHOLE call, not just the shared
                // arm B-2026-08-29-13 gated. Suppression is a HANDOVER: it
                // takes the tail's buffer away from whatever owns it so the
                // branch's consumer can own it instead. A discarded branch has
                // no consumer, so the handover strands the buffer — the same
                // reasoning the row above records for the shared ref and the
                // boxed-payload neutralizer records for its interior walk,
                // applied to every channel this call touches rather than one.
                //
                // Leaving the source armed is the entire fix for the population
                // whose tail NAMES something: a pattern binding, or a local in
                // scope. The population whose tail MINTS its value has no
                // source to leave armed and is handled separately, by giving
                // the value an owner in the arm's own frame.
                if owns_result {
                    self.suppress_source_vec_cleanup_for_arg_ex(&arm.body, owns_result);
                }
                // B-2026-08-30-2 — the match sibling of the block-tail hook in
                // `compile_block_with_frame`. A bare-identifier arm body never
                // reaches that function at all, so the disarm the call above
                // just performed has to be picked up here or the buffer it
                // stranded stays stranded: `match n { 0 => mkA(n), _ => b1 }
                // .contains("-")` leaked `b1` whole.
                let arm_disarmed = self.vecstr_source_disarmed.take();
                // B-2026-08-29-13 — the match-arm sibling of the block-tail
                // record in `suppress_block_tail_cleanup`. An arm yielding a
                // bare `shared` binding transfers the ref; without this the
                // consuming `let` took its own inc on top and the box leaked.
                // The arm tail does not route through `suppress_block_tail_cleanup`,
                // so it needs the raise here.
                if self.shared_transfer_applied {
                    self.block_tail_shared_transfer = true;
                }
                // B-2026-08-26-12 — the `Option[shared T]` arm-tail retain, the
                // match sibling of the value-position-block hook in
                // `compile_block_with_frame`. A bare `Option[shared]` binding
                // handed out as an arm's value (`show(match k { 0 => a, _ => a })`)
                // moves by RETAIN — the source keeps its queued `RcDecOption`, so
                // the escaping value needs its own `+1` or the consumer's dec and
                // the source's dec both spend the one ref the box was built with.
                //
                // Gated on `tail.is_none()`: when this match IS the function's
                // tail return, `compile_tail_final_expr` (called just above with
                // that same `tail`) already emitted the leaf inc through its
                // Identifier arm, and a second one here would strand the count
                // one above zero — a leak in place of the double free.
                if tail.is_none() {
                    self.share_option_shared_ref_for_arg(&arm.body);
                }
                // B-2026-08-07-1 — an ARM tail that hands out a boxed-payload
                // enum binding (`match k { 0 => found, _ => Option.None }` as a
                // function's tail). Third and last of the branch-leaf
                // positions, alongside the `if`-branch block tail and the
                // function-body tail; each is a distinct walk and none of them
                // sees the others' leaves. Emitted per-arm, in the arm's own
                // block, so a sibling arm that does not hand the binding out
                // still frees its box.
                if let ExprKind::Identifier(n) = &arm.body.kind {
                    let n = n.clone();
                    self.suppress_boxed_enum_payload_cleanup_for_owner(&n);
                }
                // B-2026-08-04-2 — the arm binding itself escaping as the
                // match's VALUE (`let r2 = match o { Some(r) => r, .. }`) is a
                // move like any other, but this tail position is not on
                // `suppress_inline_option_result_binding_move`'s roster, so the
                // payload-view neutralizer needs its own call here.
                //
                // B-2026-08-28-66 — through the arm's TAIL, not `arm.body`
                // itself. `suppress_boxed_payload_view_move` keys on an
                // `ExprKind::Identifier` and an arm written with braces
                // (`Some(r) => { r }`) is a Block, so it returned early and the
                // box's interior walk stayed armed beside the destination's own
                // drop: one buffer, two owners. Measured on
                // `let k = match o { Some(r) => { r } .. }` over
                // `Option[R]`, `R { id: i64, name: String }` (4 words, so the
                // payload BOXES): `karac run` aborts with a glibc double free
                // and the AOT binary reports `Invalid free()` under valgrind
                // with 11 allocs / 12 frees at -O0 — while the identical
                // program written `Some(r) => r` is clean at 11/11. The braces
                // were the whole difference.
                //
                // Same hole the f-string neutralizer below already closes for
                // its own shape, and for the same stated reason: an
                // `ExprKind::Identifier`-only guard does not see a block tail.
                // It is CLEAN at -O2, so the default leg cannot catch this.
                //
                // Gated on `branch_value_is_owned`, and that guard is REQUIRED
                // rather than defensive: neutralizing assumes the destination
                // registers a drop of its own, and a DISCARDED match result has
                // no destination at all. Without it, `match o { Some(r) => { r } .. };`
                // in statement position went from 11 allocs / 11 frees to
                // 11 / 10 with 2 bytes definitely lost — trading the double
                // free for a leak. The pre-existing bare-tail call never needed
                // the guard only because it never reached a block-tail arm.
                // The guard applies to the BARE tail as well, which fixes a
                // second, pre-existing shape rather than merely preserving it:
                // `match o { Some(r) => r, .. };` in statement position was
                // already neutralizing the box against a destination that does
                // not exist, and leaked 2 bytes (10 allocs / 9 frees) while
                // running no body on ANY backend. Both spellings now neutralize
                // exactly when something downstream owns the result.
                if self.branch_value_is_owned(scrutinee) {
                    self.suppress_boxed_payload_view_move(Self::block_tail_expr(&arm.body));
                }
                // …and the payload's BODIES walk with it. Every other move
                // position reaches `disarm_container_bodies_move_sources` (let
                // RHS, aggregate literal) or `disarm_container_bodies_for_arg`
                // (container push); the match tail reaches neither, so without
                // this the destination binding's body and the source's
                // re-homed walk both fired — two bodies for one value, the
                // second over the moved-from slot.
                //
                // B-2026-08-31-28 — gated on `owns_result`, in lockstep with
                // the memory half directly above (the B-2026-08-28-67 rule).
                // That call took the guard in B-2026-08-28-66 and this one,
                // two lines below it, did not — so a DISCARDED match still
                // retracted the source's bodies walk while handing the value
                // to nobody, and the payload's `Drop` body ran on neither
                // compiled backend:
                //
                //     let _ = match o { Some(r) => r, None => mk(9) };
                //     --interp dR1      karac run / build (nothing)
                //
                // The retraction is a HANDOVER, exactly as the memory half's
                // comment argues: it takes the body away from the source so
                // the destination's own drop can run it. With no destination
                // there is nothing to hand to, and the body is simply lost.
                //
                // Three controls say it is the intersection and not one axis:
                // the BRACED spelling of the same arm is correct (a Block is
                // not an `Identifier`, so this never fired for it and the
                // box's walk still ran the body), a USER enum is correct, and
                // an `Option` whose payload carries no heap is correct. Only a
                // boxed payload handed out of a bare arm reaches here.
                if owns_result {
                    if let ExprKind::Identifier(nm) = &arm.body.kind {
                        let nm = nm.clone();
                        self.suppress_container_elem_bodies_for_var(&nm);
                    }
                }
                // Move-aware, Map/Set variant: `match opt { Some(m) => m }`
                // returns the bound Map/Set by identity into the match result
                // (the caller now owns the handle). Retract the binding's
                // `FreeMapHandle` (queued by the match-bind `track_map_var` in
                // `bind_pattern_values`) so the per-arm drain doesn't free a
                // handle the result owner will free — the Map sibling of the
                // Vec `cap`-zeroing above (B-2026-06-12-6 cluster 4). Direct-
                // Identifier arm tail only, matching the Vec suppressor's gate.
                if let ExprKind::Identifier(nm) = &arm.body.kind {
                    let nm = nm.clone();
                    self.suppress_map_cleanup_for_tail_identifier(&nm);
                }
                // Move-aware, f-string variant: when the arm's tail
                // expression is an f-string (`Some(name) => f"[{name}]"`),
                // `arm_val` is the loaded `{data, len, cap}` of the freshly
                // built accumulator, but that acc was `track_vec_var`-
                // registered for scope cleanup — the per-arm
                // `drain_top_frame_with_emit()` below would `FreeVecBuffer`
                // its `data` between the load and the merge, so the match's
                // result (and any caller binding) sees an empty/dangling
                // String. The caller now owns the buffer, so zero the acc's
                // `cap` to no-op its cleanup — exactly the function-tail
                // f-string-return handling (`compile_function`). The
                // identifier-tail case above is covered by
                // `suppress_source_vec_cleanup_for_arg`; this covers the
                // direct- and block-tail f-string shapes it skips (its
                // `ExprKind::Identifier`-only guard returns early for an
                // `InterpolatedStringLit`).
                if Self::expr_tail_is_fstring(&arm.body) {
                    if let Some(acc) = self.last_fstr_acc.take() {
                        self.zero_vec_alloca_cap(acc);
                    }
                }
                // B-2026-09-01-10 — the BARE-bodied sibling of the arm-level
                // owner `compile_block_with_frame` gives a braced arm. When
                // this match's value is discarded AND the statement site
                // declined the construct (one arm yields a place, so freeing
                // the phi could double-free), a bare arm that MINTS its value
                // has no owner anywhere: `let _ = match o { Some(r) => r, None
                // => mk(9) };` strands `mk(9)`'s `String` on the None path,
                // 38 B per evaluation, while the braced spelling of the same
                // arm is owned. Registered in THIS arm's frame, which drains
                // immediately below, so the value dies with the arm exactly as
                // the block path arranges.
                //
                // The gate is the same `!own_value && !discard_stmt_owns_value`
                // the block path is told, and the second half is what keeps
                // this from doubling: when every arm qualifies, the statement
                // frame owns the phi and no arm may.
                if !own_value && !discard_stmt_owns_value && !body_is_block {
                    if let Some(owned_tail) = self.discarded_arm_owned_aggregate_tail(&arm.body) {
                        if self.expr_yields_fresh_owned_temp(owned_tail) {
                            self.materialize_owned_temp(
                                arm_val,
                                (owned_tail.span.offset, owned_tail.span.length),
                            );
                        }
                        self.track_discarded_arm_owned_aggregate(owned_tail, arm_val);
                    }
                }
                self.drain_top_frame_with_emit();
                // Deep-copy an owned-param arm tail (the caller retains the
                // param's buffer) so the match result owns an independent
                // buffer — the move-suppression above only skips a local
                // owner's free, leaving a param tail aliasing the caller's arg
                // and the consumer double-frees. Emit AFTER drain so the copy
                // is the escaping value; its blocks are folded into `merge_pred`
                // captured just below. No-op for local/non-param arm tails
                // (`match opt { Some(v) => v }` — `v` is a payload binding, not
                // in `owned_vecstr_params`). See `compile_if`.
                arm_val = self.deepcopy_owned_param_branch_tail(&arm.body, arm_val, own_value)?;
                // B-2026-08-30-2 — give this arm's escaping value an owner, in
                // the frame that drains after the consumer rather than the
                // arm's own (which was popped just above). Keyed by the MATCH's
                // span so an owning destination takes it over through the usual
                // funnel; a read-only one leaves it and the buffer dies at that
                // frame's exit.
                //
                // A block-bodied arm has already done this from inside
                // `compile_block_with_frame` and cleared the one-shot, which is
                // what keeps the two paths from each registering an owner for
                // the same buffer.
                // B-2026-08-30-11 — recorded, not registered: see
                // `arm_owner_records`. The store still lands in `merge_pred`
                // below, which is exactly where the immediate registration put
                // it (the builder sits there), so deferring changes WHEN the
                // decision is made and not WHERE the IR goes.
                let arm_pending_is_block = arm_pending.is_some();
                let arm_owner_pending = if own_value && !arms_all_mint {
                    arm_pending.or(arm_disarmed)
                } else {
                    None
                };
                let arm_owner_mints = own_value
                    && !arms_all_mint
                    && self.branch_tail_mints_fresh_owned_temp(&arm.body);
                // Re-read the current bb AFTER drain — the cleanup IR
                // may have appended new basic blocks (e.g. `cleanup.free`
                // / `cleanup.skip` for FreeVecBuffer's `cap > 0` guard),
                // so the merge-predecessor is the drain's exit bb, NOT
                // `arm_body_end`. The PHI at `merge_bb` must list the
                // ACTUAL predecessor bb where the unconditional branch
                // to merge originates from, or LLVM module verification
                // fails with "PHI node entries do not match predecessors".
                let merge_pred = self.builder.get_insert_block().unwrap();
                // B-2026-09-01-26 — may this arm's owner be re-homed to the
                // innermost LIVE frame when the frame its record names has
                // already drained? Three conditions, and each was established by
                // a FAILURE rather than by argument. See
                // `own_escaping_tail_value_at` for what the re-home does.
                //
                // (1) THE ARM DESTRUCTURES A VARIANT PAYLOAD. Re-homing a
                // WHOLE-VALUE rebind — `match s { t => { t } }` over a plain
                // `String` local — turned that shape from clean into
                // `free(): double free detected in tcache 2` at both opt levels:
                // the value already has an owner downstream. A payload moved OUT
                // of an enum has no second owner, because the enum's own payload
                // cleanup was disarmed by the handover and the arm-local frame
                // that recorded the binding is gone by the time this deferred
                // registration runs.
                //
                // (2) THE ARM IS BLOCK-BODIED, i.e. it reported through
                // `compile_block_with_frame`'s `arm_pending_tail_owner` rather
                // than through the bare-tail `vecstr_source_disarmed` channel.
                // This is the condition that excludes the generic-enum debox
                // shape, `fn get[T](o: Opt[T], d: T) -> T { match o { Opt.Yes(v) => v, .. } }`,
                // whose value escapes the frame entirely: without it,
                // `asan_generic_enum_heap_payload_bind_return_no_leak_or_double_free`
                // AND `selfhost_codegen_matches_seed_run` both fail. It is also
                // why the BARE arm spelling of this row's own leak stays open —
                // it needs a discriminator this record does not carry, not a
                // looser gate.
                //
                // (3) NOT THE FUNCTION'S TAIL RETURN. Belt-and-braces: condition
                // (2) already covers the escaping shape that was measured, and
                // this excludes the rest of that position on the same reasoning
                // rather than on a measurement of its own.
                let arm_rehome = arm_pending_is_block
                    && tail.is_none()
                    && self.variant_pattern_enum_name(&arm.pattern).is_some();
                arm_owner_records.push((
                    arm_owner_pending,
                    arm_val,
                    merge_pred,
                    arm_owner_mints,
                    arm_rehome,
                ));
                arm_results.push((arm_val, merge_pred));
                arm_unsigned.push(self.branch_tail_is_unsigned_int(Some(&arm.body)));
                self.builder.build_unconditional_branch(merge_bb).unwrap();
            } else {
                // Early-return / terminator inside arm body: the return
                // path's own `emit_scope_cleanup` walked the entire stack
                // including this per-arm frame and emitted cleanup for
                // its actions before the return. Pop the now-spent frame
                // so it doesn't shadow subsequent arms' bindings.
                self.drop_rc.scope_cleanup_actions.pop();
            }
            // B-2026-07-13-6: revert this arm's pattern/body binds (see the
            // per-arm snapshot above) so the next arm and the post-`match` code
            // resolve outer names, not this arm's shadows.
            self.restore_var_env(arm_snap);
        }
        self.borrow_vars.borrowed_agg_payload_struct_vars = saved_borrowed_agg_payload_vars;
        self.payload_vars.boxed_payload_alias = saved_boxed_payload_alias;

        // Wire the entry block. With a qualifying string-dispatch plan, branch
        // `entry_bb` through the switch tree straight into the arm bodies;
        // otherwise fall back to the linear cascade (entry → match.arm0). The
        // cascade's test blocks stay in place either way — when dispatch is
        // used they become unreachable and LLVM DCE drops them.
        self.builder.position_at_end(entry_bb);
        let used_dispatch = dispatch_plan
            .as_ref()
            .map(|plan| self.emit_string_dispatch(plan, scrut, &arm_body_bbs, fn_val))
            .unwrap_or(false);
        if !used_dispatch {
            self.builder.build_unconditional_branch(arm0_bb).unwrap();
        }

        // Terminate the last fail_bb (match.nofall) — exhaustive matches never
        // reach here; emit `unreachable` so LLVM doesn't require a phi entry.
        self.builder.position_at_end(next_bb);
        if next_bb.get_terminator().is_none() {
            self.builder.build_unreachable().unwrap();
        }

        // B-2026-08-30-11 — every arm is compiled, so a minting arm can now
        // borrow a sibling's frame. The first arm that reported one answers for
        // all of them: they hand out one Kāra value with one lifetime, and any
        // reporting arm's frame held a binding declared before the `match`, so
        // it outlives the construct by construction.
        if let Some(span) = self.current_branch_expr_span {
            let sibling = arm_owner_records.iter().find_map(|(p, _, _, _, _)| *p);
            for (pending, val, bb, mints, rehome) in arm_owner_records.clone() {
                let eff = self.mixed_branch_arm_owner(pending, sibling, mints, true);
                self.register_pending_arm_owner_at(
                    eff,
                    val,
                    span,
                    match_reset_bb,
                    Some(bb),
                    rehome,
                );
            }
        }

        self.builder.position_at_end(merge_bb);
        self.pattern_state.pattern_binding_is_borrow = saved_borrow_flag;
        self.pattern_state
            .pattern_binding_source_retains_inline_payload = saved_source_retains;
        self.pattern_state
            .pattern_binding_scrutinee_is_elidable_param = saved_elidable_param_flag;
        self.pattern_state
            .pattern_binding_scrutinee_is_option_result = saved_opt_res_flag;
        self.pattern_state.pattern_binding_scrutinee_optres_area = saved_optres_area;
        self.pattern_state.pattern_binding_scrutinee_is_shared_enum = saved_shared_enum_flag;
        self.pattern_state
            .pattern_binding_scrutinee_is_fresh_owning_temp = saved_fresh_temp_flag;
        self.pattern_state.pattern_binding_scrutinee_is_owned_param = saved_owned_param_flag;
        self.pattern_state
            .pattern_binding_scrutinee_is_owned_elem_clone = saved_owned_elem_clone_flag;
        self.pattern_state
            .pattern_binding_scrutinee_payload_bodies_src = saved_bodies_src;
        self.pattern_state.pattern_binding_scrutinee_optres_slot = saved_optres_slot;
        self.pattern_state.match_scrutinee_enum_hint = saved_scrut_enum_hint;

        // Every arm diverged (`return` / `unreachable()` / `todo()` in all of
        // them): no arm branched to `merge_bb`, so it has no predecessors.
        // Terminate it with `unreachable` so the enclosing fn-tail `ret` guard
        // skips emitting `ret <i64 placeholder>` against a non-i64 return type
        // (the gap-d failure class for an all-diverging `match` tail).
        if arm_results.is_empty() {
            self.builder.build_unreachable().unwrap();
            return Ok(self.context.i64_type().const_int(0, false).into());
        }

        // Reconcile narrow-int arm widths before the phi: arms that went
        // through narrow-int arithmetic (or are suffixless literals) carry i64
        // beside narrow-typed siblings of the SAME Kāra type. Truncate the wide
        // ones down so the all-same-type check below holds and the phi is built
        // rather than falling through to the const-0 placeholder. See
        // `unify_int_branch_widths` for the value-preservation invariant.
        self.unify_int_match_arm_widths(&mut arm_results);
        // Reconcile mixed float arm widths (`I(x) => x as f64` beside `F(f) =>
        // f.value` where `f: F32`) by widening every narrower float arm up to
        // the widest present — else the all-same-type check below fails and the
        // match falls to the `i64 0` placeholder (B-2026-07-23-2). See
        // `unify_float_match_arm_widths` for the widen-not-truncate rationale.
        self.unify_float_match_arm_widths(&mut arm_results);
        // B-2026-08-30-49 — the MIXED case neither width pass covers: an
        // integer arm beside a float one (`match n { 1 => i, _ => 0.0 }`)
        // failed the all-same-type check below and took the whole `match` to
        // the const-`i64 0` placeholder. Runs last, so the float arms have
        // already settled on the width the integer arms convert into.
        self.unify_int_float_match_arm_values(&mut arm_results, &arm_unsigned);

        // Build phi if all (live) arms produce a value of the same type. A
        // single live arm (the rest diverging) yields a one-incoming phi,
        // which is valid and dominates the merge — so
        // `match x { A => v, _ => unreachable() }` evaluates to `v`.
        let first_ty = arm_results[0].0.get_type();
        if arm_results.iter().all(|(v, _)| v.get_type() == first_ty) {
            let phi = self.builder.build_phi(first_ty, "matchval").unwrap();
            for (val, bb) in &arm_results {
                phi.add_incoming(&[(val, *bb)]);
            }
            let merged = phi.as_basic_value();
            // B-2026-08-28-44 — own what an arm tail handed out.
            if let Some(elem_ty) = self.branch_arm_clone_taken.take() {
                self.own_branch_merged_clone(merged, elem_ty);
            }
            // B-2026-08-29-28 — the scrutinee temporary dies HERE, at match
            // exit, per design.md § Temporary Lifetime Rules. Emitted after the
            // phi because a PHI must lead its block; that ordering is the only
            // reason this sits at the two tail returns rather than beside
            // `position_at_end(merge_bb)`.
            if let Some((alloca, _)) = freshtemp_enum {
                self.fire_freshtemp_scrutinee_body_at_exit(alloca, "__freshtemp_enum_scrut");
            }
            if let Some((alloca, _)) = freshtemp_struct {
                self.fire_freshtemp_scrutinee_body_at_exit(alloca, "__freshtemp_struct_scrut");
            }
            if let Some(ptr) = freshtemp_shared_enum {
                self.fire_freshtemp_shared_scrutinee_dec_at_exit(ptr, "__freshtemp_shared_enum");
            }
            return Ok(merged);
        }

        if let Some((alloca, _)) = freshtemp_enum {
            self.fire_freshtemp_scrutinee_body_at_exit(alloca, "__freshtemp_enum_scrut");
        }
        if let Some((alloca, _)) = freshtemp_struct {
            self.fire_freshtemp_scrutinee_body_at_exit(alloca, "__freshtemp_struct_scrut");
        }
        if let Some(ptr) = freshtemp_shared_enum {
            self.fire_freshtemp_shared_scrutinee_dec_at_exit(ptr, "__freshtemp_shared_enum");
        }
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    /// Decide whether a `match`'s arms are a pure string-literal dispatch the
    /// switch tree can lower (see [`StringDispatchPlan`]). Conservative — any
    /// shape it can't prove equivalent to the cascade returns `None` and the
    /// cascade handles it:
    /// - every non-last arm must be a bare `Literal::String` with no guard;
    /// - the last arm may also be a string literal (no catch-all → the tree's
    ///   default is `unreachable`, matching the cascade's `match.nofall`), or a
    ///   `Wildcard` / plain `Binding` catch-all;
    /// - duplicate literals bail (the cascade's first-match-wins could differ);
    /// - `Or`-patterns, range/struct/variant patterns, and guards all bail.
    fn analyze_string_dispatch(&self, arms: &[MatchArm]) -> Option<StringDispatchPlan> {
        if arms.len() < STRING_DISPATCH_MIN_ARMS {
            return None;
        }
        let last = arms.len() - 1;
        let mut string_arms: Vec<(String, usize)> = Vec::new();
        let mut default_arm: Option<usize> = None;
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for (i, arm) in arms.iter().enumerate() {
            // A guard adds a runtime condition the switch tree can't express.
            if arm.guard.is_some() {
                return None;
            }
            match &arm.pattern.kind {
                PatternKind::Literal(LiteralPattern::String(s)) => {
                    if !seen.insert(s.as_str()) {
                        return None;
                    }
                    string_arms.push((s.clone(), i));
                }
                // A trailing wildcard / plain binding is the catch-all. Only the
                // LAST arm may be one; a non-string, non-last arm disqualifies.
                PatternKind::Wildcard | PatternKind::Binding(_) if i == last => {
                    // A `Binding` whose name is a unit enum variant is a tag
                    // test, not a catch-all — bail. (Defensive: a String
                    // scrutinee has no variants, but keep the analyzer honest.)
                    if let PatternKind::Binding(name) = &arm.pattern.kind {
                        let variant = name.rsplit('.').next().unwrap_or(name);
                        if self.enum_tag_for_variant(variant).is_some() {
                            return None;
                        }
                    }
                    default_arm = Some(i);
                }
                _ => return None,
            }
        }
        if string_arms.len() < STRING_DISPATCH_MIN_ARMS {
            return None;
        }
        Some(StringDispatchPlan {
            arms: string_arms,
            default_arm,
        })
    }

    /// Emit the string-dispatch switch tree at the current insert point (the
    /// match's entry block). Branches the entry through `switch len → switch
    /// first-byte → residual memcmp` straight into the arm body blocks the
    /// cascade already built (`arm_body_bbs`). Returns `false` (caller falls
    /// back to the cascade entry branch) only if `scrut` isn't the expected
    /// String `{ ptr, len, cap }` struct value.
    fn emit_string_dispatch(
        &self,
        plan: &StringDispatchPlan,
        scrut: BasicValueEnum<'ctx>,
        arm_body_bbs: &[BasicBlock<'ctx>],
        fn_val: FunctionValue<'ctx>,
    ) -> bool {
        let BasicValueEnum::StructValue(sv) = scrut else {
            return false;
        };
        let i64_t = self.context.i64_type();
        let entry_bb = self.builder.get_insert_block().unwrap();

        // Default target: the catch-all arm's body, or a fresh `unreachable`
        // block (a String `match` with no catch-all is non-exhaustive — the
        // typechecker rejects it — but stay defensive rather than assume).
        let default_bb = match plan.default_arm {
            Some(idx) => arm_body_bbs[idx],
            None => {
                let ub = self.context.append_basic_block(fn_val, "match.strdisp.ub");
                self.builder.position_at_end(ub);
                self.builder.build_unreachable().unwrap();
                self.builder.position_at_end(entry_bb);
                ub
            }
        };

        let scrut_ptr = self
            .builder
            .build_extract_value(sv, 0, "sd.ptr")
            .unwrap()
            .into_pointer_value();
        let scrut_len = self
            .builder
            .build_extract_value(sv, 1, "sd.len")
            .unwrap()
            .into_int_value();

        // Group keyword arms by byte length (BTreeMap → deterministic IR).
        let mut by_len: std::collections::BTreeMap<usize, Vec<(&str, usize)>> =
            std::collections::BTreeMap::new();
        for (kw, idx) in &plan.arms {
            by_len
                .entry(kw.len())
                .or_default()
                .push((kw.as_str(), *idx));
        }

        let mut len_cases: Vec<(IntValue<'ctx>, BasicBlock<'ctx>)> =
            Vec::with_capacity(by_len.len());
        for (len, kws) in &by_len {
            let len_bb = self
                .context
                .append_basic_block(fn_val, &format!("match.strdisp.len{}", len));
            len_cases.push((i64_t.const_int(*len as u64, false), len_bb));
            self.builder.position_at_end(len_bb);
            self.emit_len_bucket(*len, kws, scrut_ptr, default_bb, arm_body_bbs, fn_val);
        }

        self.builder.position_at_end(entry_bb);
        self.builder
            .build_switch(scrut_len, default_bb, &len_cases)
            .unwrap();
        true
    }

    /// One length bucket of the string-dispatch tree. All `kws` share `len`.
    /// Builder is positioned at the bucket's block on entry. For `len == 0`
    /// (only the empty string) or `len == 1` (first byte uniquely identifies
    /// the arm) it branches directly; otherwise it switches on the first byte.
    fn emit_len_bucket(
        &self,
        len: usize,
        kws: &[(&str, usize)],
        scrut_ptr: PointerValue<'ctx>,
        default_bb: BasicBlock<'ctx>,
        arm_body_bbs: &[BasicBlock<'ctx>],
        fn_val: FunctionValue<'ctx>,
    ) {
        if len == 0 {
            // Only the empty string lands here, so at most one keyword arm.
            let target = kws
                .first()
                .map(|(_, idx)| arm_body_bbs[*idx])
                .unwrap_or(default_bb);
            self.builder.build_unconditional_branch(target).unwrap();
            return;
        }

        let i8_t = self.context.i8_type();
        let first_byte = self
            .builder
            .build_load(i8_t, scrut_ptr, "sd.fb")
            .unwrap()
            .into_int_value();

        let mut by_byte: std::collections::BTreeMap<u8, Vec<(&str, usize)>> =
            std::collections::BTreeMap::new();
        for (kw, idx) in kws {
            by_byte
                .entry(kw.as_bytes()[0])
                .or_default()
                .push((*kw, *idx));
        }

        let len_bb = self.builder.get_insert_block().unwrap();
        let mut byte_cases: Vec<(IntValue<'ctx>, BasicBlock<'ctx>)> =
            Vec::with_capacity(by_byte.len());
        for (byte, group) in &by_byte {
            let byte_bb = self
                .context
                .append_basic_block(fn_val, &format!("match.strdisp.b{}", byte));
            byte_cases.push((i8_t.const_int(u64::from(*byte), false), byte_bb));
            self.builder.position_at_end(byte_bb);
            self.emit_byte_group(len, group, scrut_ptr, default_bb, arm_body_bbs, fn_val);
        }

        self.builder.position_at_end(len_bb);
        self.builder
            .build_switch(first_byte, default_bb, &byte_cases)
            .unwrap();
    }

    /// Residual confirmation for one `(len, first_byte)` group. We reached this
    /// block via the length switch, so `scrut_len == len` exactly — `memcmp`
    /// reads exactly `len` valid bytes from both operands (no length re-check,
    /// no over-read). `len == 1` needs no `memcmp` (the byte switch already
    /// confirmed the sole byte, and distinct len-1 keywords have distinct first
    /// bytes → the group is a single arm). Otherwise chain `memcmp`-equals over
    /// the candidates, falling through to `default_bb`.
    fn emit_byte_group(
        &self,
        len: usize,
        group: &[(&str, usize)],
        scrut_ptr: PointerValue<'ctx>,
        default_bb: BasicBlock<'ctx>,
        arm_body_bbs: &[BasicBlock<'ctx>],
        fn_val: FunctionValue<'ctx>,
    ) {
        if len == 1 {
            let (_, idx) = group[0];
            self.builder
                .build_unconditional_branch(arm_body_bbs[idx])
                .unwrap();
            return;
        }

        let i32_t = self.context.i32_type();
        let i64_t = self.context.i64_type();
        let len_v = i64_t.const_int(len as u64, false);
        for (n, (kw, idx)) in group.iter().enumerate() {
            let body_bb = arm_body_bbs[*idx];
            let is_last = n + 1 == group.len();
            let kw_ptr = self
                .builder
                .build_global_string_ptr(kw, "sd.kw")
                .unwrap()
                .as_pointer_value();
            let cmp = self
                .builder
                .build_call(
                    self.runtime_fns.memcmp_fn,
                    &[scrut_ptr.into(), kw_ptr.into(), len_v.into()],
                    "sd.memcmp",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let eq = self
                .builder
                .build_int_compare(IntPredicate::EQ, cmp, i32_t.const_int(0, false), "sd.eq")
                .unwrap();
            let next_bb = if is_last {
                default_bb
            } else {
                self.context
                    .append_basic_block(fn_val, "match.strdisp.next")
            };
            self.builder
                .build_conditional_branch(eq, body_bb, next_bb)
                .unwrap();
            if !is_last {
                self.builder.position_at_end(next_bb);
            }
        }
    }

    /// Whether an expression's syntactic tail value is an f-string. A bare
    /// `InterpolatedStringLit` is the tail; a block (`{ …; f"…" }`) recurses
    /// into its `final_expr`. Used by `compile_match` to detect a match-arm
    /// whose value is a freshly-built f-string accumulator, so the acc's
    /// scope cleanup can be no-op'd (ownership transferred to the match
    /// result). Conservative — an `if`/`match` tail whose branches are
    /// f-strings is NOT unwrapped (the value flows through nested phis); not
    /// matching there leaves the prior behavior, never a double-free.
    fn expr_tail_is_fstring(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::InterpolatedStringLit(_) => true,
            ExprKind::Block(b) => b
                .final_expr
                .as_deref()
                .is_some_and(Self::expr_tail_is_fstring),
            _ => false,
        }
    }

    /// True when a match scrutinee expression's value aliases a container
    /// the surrounding scope still owns — and so the cleanup actions
    /// attached to that container will free any heap-bearing payload words
    /// embedded in the scrutinee's value. In those cases, a pattern
    /// binding extracted from the scrutinee must NOT itself register a
    /// cleanup, or the buffer will be freed twice.
    ///
    /// Current closed list (returns by value, container retains
    /// ownership): `Map.get`. Other shape candidates (`Vec.first`,
    /// `Vec.last`, `Slice.get`, ...) are followups — they return one-word
    /// scalar payloads in the v1 stdlib, not heap-bearing Vec/String, so
    /// their match-arm bindings don't trigger the duplicate cleanup yet.
    /// `Map.remove` truly transfers ownership (the entry is deleted) and
    /// is intentionally NOT on this list — its `Some(v)` bindings still
    /// own the Vec they receive.
    ///
    /// Receiver-aware: a `.get()` is classified as a borrow-return *unless*
    /// its receiver is the HTTP `Client`. `Client.get(url)` is a GET request
    /// that returns a freshly-**owned** `Result[Response, HttpError]` — same
    /// method name, opposite ownership from a collection accessor. Classifying
    /// it as a borrow suppresses the Response/HttpError scope-exit Drop and
    /// leaks the body `String` buffer, the headers side-table handle, and the
    /// `HttpError.message` buffer (B-2026-06-10-3 — the name-only heuristic
    /// regressed these). Every other `.get()` keeps the conservative borrow
    /// classification, so the `Map.get` double-free protection is unchanged.
    /// True when the scrutinee is a borrowed binding — a `ref`/`mut ref` param,
    /// including `ref self` (the `Display::to_string(ref self)` case). Matching
    /// a borrow transfers no ownership, so a payload-field binding must alias
    /// the source storage rather than register its own `FreeVecBuffer` cleanup
    /// — otherwise it double-frees the heap payload against the owner's drop
    /// (the Weave dogfood's `ParseError` Display, matching a struct-variant
    /// `String` payload through `ref self`). The receiver-side counterpart of
    /// `scrutinee_is_borrow_call` (borrow-RETURNING accessors like `Map.get`).
    /// Also true for a `for`-loop ELEMENT binding whose container's
    /// per-element drop is armed (`for_loop_borrow_vars` — heap Vec/String
    /// elements, and slice 3q's Option/Result-with-heap-payload elements):
    /// the loop var is a bit-copy of the container's element, so a
    /// payload-consuming arm must alias, not own — the container's element
    /// drop is the single owner.
    pub(super) fn scrutinee_is_borrowed_binding(&self, scrutinee: &Expr) -> bool {
        let name = match &scrutinee.kind {
            ExprKind::SelfValue => "self",
            ExprKind::Identifier(n) => n.as_str(),
            _ => return false,
        };
        self.borrow_vars.ref_params.contains_key(name)
            || self.borrow_vars.for_loop_borrow_vars.contains(name)
            || self
                .borrow_vars
                .borrow_accessor_let_payload
                .contains_key(name)
    }

    /// True when the scrutinee is a **bare identifier naming a param already in
    /// `rc_elide_ref_params`** for the current function — a read-only,
    /// non-escaping borrowed `shared`/`Option[shared]` param (B-2026-07-15-21
    /// Part B). `rc_elide.rs`'s four conditions (incl. condition 4,
    /// `payloads_never_move_out`) have proven the param's payload is
    /// projection-only, so the `Some(n)` binding never escapes and its retain +
    /// scope-exit `RcDec` are a balanced no-op safe to skip. Bare identifier
    /// only (a projection like `n.left` is a different, non-param place).
    pub(super) fn scrutinee_is_elidable_param(&self, scrutinee: &Expr) -> bool {
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return false;
        };
        self.drop_rc
            .rc_elide_ref_params
            .get(&self.fn_ctx.current_fn_name)
            .is_some_and(|recs| recs.iter().any(|(n, _)| n == name))
    }

    /// True when the scrutinee is a **field/element read through a borrow**
    /// (`h.w` / `h.a.b` / `h.xs[i]` where the root binding `h` is a `ref` /
    /// `mut ref` / for-loop-borrow / borrow-accessor-let) AND no arm ever
    /// moves a payload binding out of its body/guard.
    ///
    /// Such a read aliases the owner's storage and performs no deep copy (an
    /// OWNED root's field access deep-copies instead — that path owns its
    /// payload and drops it safely). So a payload binding here is a read-only
    /// VIEW of storage the caller still owns: registering its own scope-exit
    /// drop double-frees against the owner (B-2026-07-11-12 — `match
    /// h.w { Some(g) => for x in g.items { … } }`, `h: ref Holder`). Classing
    /// the scrutinee as a borrow suppresses that drop.
    ///
    /// The escape guard is the safety boundary: when an arm MOVES a payload out
    /// (`match self.toks[i].tok { Id(s) => s }` — `s` is returned), the borrow
    /// path's clone-on-escape net only covers `Option`/`Map.get` scrutinees, so
    /// a moved user-enum payload would alias freed storage. Those cases stay on
    /// the existing owned move-out path (source-payload cleanup suppression),
    /// which already handles the escape — so this returns false for them.
    fn scrutinee_is_readonly_borrowed_place(&self, scrutinee: &Expr, arms: &[MatchArm]) -> bool {
        // Only place expressions — bare identifiers / `self` are handled by
        // `scrutinee_is_borrowed_binding` (and route through the pointer-source
        // `bind_pattern_values_via_ptr` path, not this value path).
        if !matches!(
            scrutinee.kind,
            ExprKind::FieldAccess { .. } | ExprKind::Index { .. } | ExprKind::TupleIndex { .. }
        ) {
            return false;
        }
        let Some(root) = self.place_expr_root_ident(scrutinee) else {
            return false;
        };
        if !self.scrutinee_is_borrowed_binding(root) {
            return false;
        }
        self.no_arm_payload_escapes(arms)
    }

    /// Does every payload binding this arm's pattern makes get used ONLY as the
    /// scrutinee of a nested `match` / `if let` (B-2026-08-30-52 (b))?
    ///
    /// The arm must bind at least one payload name, and each such name must be
    /// mentioned at least once — a bound-but-unused payload answers `false` and
    /// keeps the path it takes today, which is the conservative direction and
    /// costs nothing, since that arm is not the shape that breaks.
    fn arm_payload_binding_only_feeds_nested_match(&self, pattern: &Pattern, body: &Expr) -> bool {
        let mut names: Vec<String> = Vec::new();
        Self::collect_variant_payload_binding_names(pattern, false, &mut names);
        !names.is_empty()
            && names
                .iter()
                .all(|n| crate::binding_use::binding_only_nested_match_scrutinee(n, body))
    }

    /// The `if let` / `while let` sibling of
    /// [`Self::arm_payload_binding_only_feeds_nested_match`], for the shapes
    /// whose bindings live in a block rather than one arm expression.
    fn pattern_payload_binding_only_feeds_nested_match_block(
        &self,
        pattern: &Pattern,
        block: &crate::ast::Block,
    ) -> bool {
        let mut names: Vec<String> = Vec::new();
        Self::collect_variant_payload_binding_names(pattern, false, &mut names);
        !names.is_empty()
            && names
                .iter()
                .all(|n| crate::binding_use::binding_only_nested_match_scrutinee_block(n, block))
    }

    /// B-2026-08-30-52 (b), the second half: PROPAGATE the outer arm's
    /// borrow-ness into a nested `match` over the binding it made.
    ///
    /// `register_borrowed_agg_payload_struct_bindings` already records every
    /// borrow-mode payload binding in `borrowed_agg_payload_struct_vars` — a
    /// set that means precisely "this binding aliases storage someone else
    /// owns". An inner `match` over such a binding is therefore a match over a
    /// borrow, and must not register owners for the leaves it destructures;
    /// without this the inner arm took the payload out from under a source that
    /// still owned it (`Result` twin: `inner` then an EMPTY line).
    ///
    /// The row records that teaching `scrutinee_is_borrowed_binding` about this
    /// set was measured NOT to fix the bug on its own, and that stands: the
    /// OUTER classifier declined first, so nothing was ever registered to find
    /// here. The escape-walk refinement above is what makes the registration
    /// happen; this is what consumes it. Neither half works alone.
    ///
    /// Kept as its own disjunct rather than folded into
    /// `scrutinee_is_borrowed_binding` because that predicate has nine other
    /// callers that ask it without any arm context, and the read-only gate
    /// (`no_arm_payload_escapes`) is not optional here — an inner arm that
    /// MOVES a leaf out genuinely needs the transfer path.
    fn scrutinee_is_readonly_borrow_mode_payload_binding(
        &self,
        scrutinee: &Expr,
        arms: &[MatchArm],
    ) -> bool {
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return false;
        };
        if !self
            .borrow_vars
            .borrowed_agg_payload_struct_vars
            .contains(name.as_str())
        {
            return false;
        }
        self.no_arm_payload_escapes(arms)
    }

    /// B-2026-08-31-23 — record that this arm's WHOLE-payload binding aliases
    /// the box interior of `scrutinee`, so a nested `match` that moves a field
    /// out of the binding can disarm the BOX rather than only the bit copy.
    ///
    /// Deliberately narrow. The alias is recorded only for a bare-identifier
    /// scrutinee that is a live boxed payload owner, a `Some`/`Ok`/`Err`
    /// pattern with exactly one sub-pattern, and that sub-pattern a plain
    /// `Binding` — a destructure is the other disarmer's job and a wildcard
    /// binds nothing, so in both of those the box must keep owning its fields.
    fn register_boxed_payload_alias(&mut self, scrutinee: &Expr, pattern: &Pattern) {
        let ExprKind::Identifier(src) = &scrutinee.kind else {
            return;
        };
        if !self
            .payload_vars
            .boxed_enum_payload_vars
            .contains(src.as_str())
        {
            return;
        }
        let PatternKind::TupleVariant { path, patterns } = &pattern.kind else {
            return;
        };
        let container = match path.last().map(|s| s.as_str()) {
            Some("Some") => "Option",
            Some("Ok") | Some("Err") => "Result",
            _ => return,
        };
        let [sub] = patterns.as_slice() else {
            return;
        };
        let PatternKind::Binding(bound) = &sub.kind else {
            return;
        };
        self.payload_vars
            .boxed_payload_alias
            .insert(bound.clone(), (src.clone(), container.to_string()));
    }

    /// The box-interior half of [`Self::suppress_destructured_enum_payload_cleanup`]
    /// for a scrutinee that is a whole-payload ALIAS (B-2026-08-31-23).
    ///
    /// Re-derives the box pointer from the SOURCE variable's slot rather than
    /// caching a `PointerValue` at the bind site, so there is no dominance
    /// question about where the value was produced.
    fn suppress_aliased_boxed_payload_cleanup(&mut self, scrut_name: &str, pattern: &Pattern) {
        let Some((src, container)) = self
            .payload_vars
            .boxed_payload_alias
            .get(scrut_name)
            .cloned()
        else {
            return;
        };
        let Some(enum_name) = self.var_types.var_type_names.get(scrut_name).cloned() else {
            return;
        };
        let Some(layout) = self
            .type_decls
            .enum_layouts
            .get(container.as_str())
            .cloned()
        else {
            return;
        };
        let Some(slot) = self.variables.get(src.as_str()).map(|s| s.ptr) else {
            return;
        };
        let Some(fn_val) = self.current_fn else {
            return;
        };
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let Ok(w0_ptr) =
            self.builder
                .build_struct_gep(layout.llvm_type, slot, 1, "boxalias.suppress.w0")
        else {
            return;
        };
        let w0 = self
            .builder
            .build_load(self.context.i64_type(), w0_ptr, "boxalias.suppress.w0v")
            .unwrap()
            .into_int_value();
        let box_ptr = self
            .builder
            .build_int_to_ptr(w0, ptr_ty, "boxalias.suppress.box")
            .unwrap();
        let is_null = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                box_ptr,
                ptr_ty.const_null(),
                "boxalias.suppress.isnull",
            )
            .unwrap();
        let do_bb = self
            .context
            .append_basic_block(fn_val, "boxalias.suppress.do");
        let join_bb = self
            .context
            .append_basic_block(fn_val, "boxalias.suppress.join");
        self.builder
            .build_conditional_branch(is_null, join_bb, do_bb)
            .unwrap();
        self.builder.position_at_end(do_bb);
        self.suppress_destructured_enum_payload_cleanup_at(box_ptr, &enum_name, pattern);
        self.builder.build_unconditional_branch(join_bb).unwrap();
        self.builder.position_at_end(join_bb);
    }

    /// Register the STRUCT payload binding(s) of a match arm's pattern into
    /// `borrowed_agg_payload_struct_vars` (B-2026-07-17-20). Called only on the
    /// borrow path (`pattern_binding_is_borrow`), where the binding aliases a
    /// container-owned aggregate; a Vec/String field copied out of it then
    /// deep-copies via `deep_copy_owned_struct_param_field_move`. Restricted to
    /// bindings whose bound value is an LLVM struct (a payload with fields to
    /// copy out); non-struct payloads (`Fu(i)`) can never be a field-access
    /// source, so they are skipped to keep the set tight.
    pub(super) fn register_borrowed_agg_payload_struct_bindings(&mut self, pattern: &Pattern) {
        let mut names: Vec<String> = Vec::new();
        Self::collect_variant_payload_binding_names(pattern, false, &mut names);
        // Register the payload binding name(s). A borrow-mode payload binding is
        // materialized as a POINTER alias into the enum payload (not a struct
        // VALUE), so no `StructType` gate is applied here — the field-access RHS
        // gate in `deep_copy_owned_struct_param_field_move` (RHS is `f.field`,
        // dest is a Vec) already makes a non-struct payload (`Fu(i)`) inert.
        for n in names {
            if self.variables.contains_key(n.as_str()) {
                self.borrow_vars.borrowed_agg_payload_struct_vars.insert(n);
            }
        }
    }

    /// Collect `Binding` leaf names that sit in a variant/struct PAYLOAD
    /// position (inside a `TupleVariant` / `Struct` pattern), skipping a
    /// top-level whole-scrutinee binding (`other => …`). `in_payload` becomes
    /// true once a variant/struct sub-pattern is entered.
    pub(super) fn collect_variant_payload_binding_names(
        pattern: &Pattern,
        in_payload: bool,
        out: &mut Vec<String>,
    ) {
        match &pattern.kind {
            PatternKind::Binding(n) if in_payload => out.push(n.clone()),
            PatternKind::Binding(_) => {}
            PatternKind::AtBinding { name, pattern, .. } => {
                if in_payload {
                    out.push(name.clone());
                }
                Self::collect_variant_payload_binding_names(pattern, in_payload, out);
            }
            PatternKind::TupleVariant { patterns, .. } => {
                for p in patterns {
                    Self::collect_variant_payload_binding_names(p, true, out);
                }
            }
            PatternKind::Struct { fields, .. } => {
                for f in fields {
                    if let Some(sub) = &f.pattern {
                        Self::collect_variant_payload_binding_names(sub, true, out);
                    } else {
                        // Field shorthand `Struct { x }` binds `x` directly.
                        out.push(f.name.clone());
                    }
                }
            }
            PatternKind::Tuple(ps) | PatternKind::Or(ps) => {
                for p in ps {
                    Self::collect_variant_payload_binding_names(p, in_payload, out);
                }
            }
            _ => {}
        }
    }

    /// The binding names this pattern introduces from BARE-TUPLE element
    /// positions — the exact complement of
    /// [`Self::collect_variant_payload_binding_names`], which walks the same
    /// tree and deliberately skips these (a tuple scrutinee's element walk
    /// stays armed and must remain the single body owner, so a tuple element
    /// must never reach the UserDrop arm channel).
    ///
    /// B-2026-08-31-7 — being excluded from that set is right for the ARM
    /// CHANNEL and wrong for VIEW-NESS, and the two had been conflated.
    /// `match t { (r, k) => { let m = r; … } }` over `fn s1(t: (R, i64))`
    /// binds `r` out of the callee's entry copy on exactly the terms an enum
    /// payload binds out of `match e { E.A(r) => … }`: the caller retains, the
    /// tuple's element walk is the single fire. But `param_view_locals` was
    /// only ever written from the variant-payload site, so `let m = r` reached
    /// the let site's `rhs_is_param_view` test knowing nothing about `r`, took
    /// the full `track_user_drop_var` channel, and ran the body the element
    /// walk was already running — measured `b1 dR1 dR1` on all three compiled
    /// backends against `--interp`'s `b1 dR1`, and `b7 dR7 dR7` on ALL FOUR
    /// surfaces for the projected spelling `match s.t { (r, k) => … }` over
    /// `fn s1c(s: W)`. The same arm WITHOUT the rebind is correct everywhere,
    /// which is what puts the axis on the rebind rather than on the pattern.
    ///
    /// Feeding these names to `param_view_locals` — and NOT to
    /// `current_variant_payload_bindings` — is what separates the two
    /// questions: the walk keeps the body (unchanged), and a rebind of the
    /// element inherits view-ness instead of minting a second owner. The enum
    /// twins of both shapes already answer one on every surface, so this
    /// brings the tuple family level with them rather than inventing a rule.
    ///
    /// Stops at `TupleVariant` / `Struct`: bindings under those ARE payload
    /// positions and the sibling collector owns them, so the two sets stay
    /// disjoint by construction.
    pub(super) fn collect_bare_tuple_binding_names(
        pattern: &Pattern,
        in_bare_tuple: bool,
        out: &mut Vec<String>,
    ) {
        match &pattern.kind {
            PatternKind::Binding(n) if in_bare_tuple => out.push(n.clone()),
            PatternKind::AtBinding { name, pattern, .. } => {
                if in_bare_tuple {
                    out.push(name.clone());
                }
                Self::collect_bare_tuple_binding_names(pattern, in_bare_tuple, out);
            }
            PatternKind::Tuple(ps) => {
                for p in ps {
                    Self::collect_bare_tuple_binding_names(p, true, out);
                }
            }
            PatternKind::Or(ps) => {
                for p in ps {
                    Self::collect_bare_tuple_binding_names(p, in_bare_tuple, out);
                }
            }
            _ => {}
        }
    }

    /// B-2026-09-02-27 / B-2026-09-02-34 — record, for each bare-tuple element
    /// binding
    /// this arm just bound, the pointer at which the SOURCE tuple's element
    /// really lives.
    ///
    /// B-2026-09-02-23 made the tuple's own `__karac_drop_tuple_*` the single
    /// owner of every element, which makes the arm binding a bit-copy VIEW. A
    /// per-field move-out (`let n = r.name`) then cap-zeroes the view — storage
    /// no drop reads — while the tuple's walk still finds a live `cap` at
    /// `t.0.name` and frees the buffer `n` now owns: `free(): double free
    /// detected in tcache 2` on jit / `-O0` / `-O2` / no-auto-par, against a
    /// correct `--interp`. Measured for a tuple PARAM, a tuple LOCAL, and a
    /// tuple STRUCT FIELD; the `let (r, k) = t;` spelling is correct on all
    /// four because `finish_place_source_tuple_destructure` transfers the
    /// element outright there.
    ///
    /// This is the mirror `zero_struct_field_move_cap_impl`'s deboxed-payload
    /// caller already performs for the box: across a COPY there is no cleanup
    /// action to retract, only data the owner's drop will read, so the move
    /// site has to write the neutralization in both places. A field the move
    /// did NOT take keeps its live `cap` and the tuple still frees it — which
    /// is what keeps this narrower than retracting the element's walk (the
    /// whole-element question is B-2026-09-02-24's).
    ///
    /// BARE-TUPLE nesting only, but to any depth: `((r, j), k)` recurses
    /// through the inner tuple's own GEP, because the element of an element is
    /// still addressable. A binding under a `TupleVariant` / `Struct`
    /// sub-pattern is NOT — that is a payload position with its own owner — so
    /// the walk stops there, exactly where `collect_bare_tuple_binding_names`
    /// stops. `field_chain_place_ptr` bails on a `ref` root, which is the same
    /// rule from the other direction: a borrowed source's owner is the caller
    /// and the callee must not write into it.
    pub(super) fn record_bare_tuple_elem_sources(&mut self, pattern: &Pattern, scrutinee: &Expr) {
        if !matches!(&pattern.kind, PatternKind::Tuple(_)) {
            return;
        }
        let Some(tuple_ty) = self.place_chain_aggregate_llvm_type(scrutinee) else {
            return;
        };
        let Some(base) = self.field_chain_place_ptr(scrutinee) else {
            return;
        };
        self.record_bare_tuple_elem_sources_at(pattern, base, tuple_ty);
    }

    /// The recursive half of [`Self::record_bare_tuple_elem_sources`], carrying
    /// the pointer and LLVM type of the tuple this level addresses.
    fn record_bare_tuple_elem_sources_at(
        &mut self,
        pattern: &Pattern,
        base: PointerValue<'ctx>,
        tuple_ty: StructType<'ctx>,
    ) {
        let PatternKind::Tuple(pats) = &pattern.kind else {
            return;
        };
        for (idx, p) in pats.iter().enumerate() {
            let Ok(elem_ptr) = self
                .builder
                .build_struct_gep(tuple_ty, base, idx as u32, "btup.src")
            else {
                continue;
            };
            match &p.kind {
                PatternKind::Tuple(_) => {
                    if let Some(BasicTypeEnum::StructType(inner)) =
                        tuple_ty.get_field_type_at_index(idx as u32)
                    {
                        self.record_bare_tuple_elem_sources_at(p, elem_ptr, inner);
                    }
                }
                PatternKind::Binding(name) => {
                    let Some(slot) = self.variables.get(name.as_str()).copied() else {
                        continue;
                    };
                    // Only an element the arm holds INLINE. A pointer slot is
                    // either a borrow shim that already aliases the source
                    // (nothing to mirror) or an RC box the refcount machinery
                    // owns.
                    if !matches!(slot.ty, BasicTypeEnum::StructType(_)) {
                        continue;
                    }
                    if elem_ptr == slot.ptr {
                        continue;
                    }
                    self.payload_vars
                        .bare_tuple_elem_slots
                        .insert((name.clone(), slot.ptr), elem_ptr);
                }
                _ => {}
            }
        }
    }

    /// True when the scrutinee is a bare `for`-loop element binding whose
    /// container element is a heap-bearing user STRUCT / ENUM
    /// (`for_loop_owned_agg_vars`) AND no arm moves a payload binding out.
    ///
    /// Such an element is registered under the deep-copy-on-whole-move
    /// ("callee-entry-copy") model rather than `for_loop_borrow_vars`, so
    /// `scrutinee_is_borrowed_binding` misses it — yet the loop var is still a
    /// bit-copy alias of the container slot whose heap the container's
    /// per-element drop owns. A `match it { A(x) => … }` that binds a payload
    /// out of that element and never escapes it is a read-only VIEW: registering
    /// x's own scope-exit drop double-frees against the element drop
    /// (`for it in ref items { match it { A(x) => … } }`, `items: Vec[MyEnum]`).
    /// Classing it as a borrow suppresses that drop. An ESCAPING payload stays
    /// on the owned path (the whole-move deep-copy model), so the escape guard —
    /// shared with `scrutinee_is_readonly_borrowed_place` — returns false there.
    fn scrutinee_is_readonly_owned_agg_loop_var(
        &self,
        scrutinee: &Expr,
        arms: &[MatchArm],
    ) -> bool {
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return false;
        };
        if !self
            .borrow_vars
            .for_loop_owned_agg_vars
            .contains(name.as_str())
        {
            return false;
        }
        self.no_arm_payload_escapes(arms)
    }

    /// B-2026-08-08-25 leg 3 — the USER-ENUM sibling of the two classifiers
    /// above: a read-only match over a bare local owning a heap-bearing
    /// value enum.
    ///
    /// `enum E { A(String), B }` matched twice printed `hi` and then an EMPTY
    /// line, because the first match's `suppress_destructured_enum_payload_
    /// cleanup` zeroed the source's payload words to hand the buffer to the arm
    /// binding — correct when the arm really takes ownership, wrong when it only
    /// reads. This is the same defect the `Option`/`Result` channel had; that
    /// channel is keyed on `inline_{option,result}_payload_vars` and so never
    /// reached a user enum, whose payload rides `field_drop_kinds` instead.
    ///
    /// Raising `pattern_binding_is_borrow` fixes both halves at once, exactly as
    /// it does for the loop-var sibling: the arm-loop suppression block is
    /// gated on that flag, so the source keeps its payload words, and
    /// `bind_pattern_values` skips the binding's own drop registration. One
    /// buffer, one free, at the source's scope exit.
    ///
    /// The source must PROVABLY still own the payload, or suppressing the arm's
    /// registration would leak it — so this requires a live `EnumDrop` on the
    /// variable's own slot rather than inferring ownership from the shape.
    /// `track_enum_var` declines to register one for a shared enum and for an
    /// enum with no heap-bearing field anywhere, which is precisely the
    /// population where a borrow classification would have nobody to fall back
    /// on. An ESCAPING arm keeps today's transfer path (`no_arm_payload_
    /// escapes` defaults to "escapes" for any shape it does not recognise, so
    /// an unrecognised body is conservative here too).
    fn scrutinee_is_readonly_owned_enum_local(&self, scrutinee: &Expr, arms: &[MatchArm]) -> bool {
        let Some(enum_name) = self.owned_enum_local_that_frees_its_own_payload(scrutinee) else {
            return false;
        };
        // At least one arm must bind a field the suppressor would zero —
        // otherwise this match is not the shape that breaks, and there is no
        // reason to move it off its current path.
        if !arms
            .iter()
            .any(|a| self.enum_pattern_binds_heap_payload(&enum_name, &a.pattern))
        {
            return false;
        }
        self.no_arm_payload_escapes(arms)
    }

    /// B-2026-08-29-29 — the PROJECTION-PLACE sibling of
    /// [`Self::scrutinee_is_readonly_owned_enum_local`]: `match s.e { … }` and
    /// `match t.0 { … }`, where the matched enum is a FIELD or TUPLE ELEMENT of
    /// a value the enclosing scope still owns.
    ///
    /// A projection is a VIEW. The owner (`s`, `t`) is untouched by the match
    /// and outlives it, so its bodies walk is the payload's one owner — which
    /// is why this returns a BORROW classification rather than trying to retract
    /// half of that walk. The retraction is not merely harder, it is not
    /// expressible: a struct's field-bodies walk is ONE emitted function running
    /// the enum's own body and its payload's body together, so
    /// `suppress_struct_field_bodies_for_var` can only take both (losing `dE`)
    /// or neither. The identifier spelling has two separate actions —
    /// `OwnWrapper` and `ContainerElemBodies` — and that, not a difference in
    /// semantics, is the whole reason it can hand the payload to the arm.
    ///
    /// Without this the arm registered the payload's body AND the owner's walk
    /// ran it, so both compiled backends fired it TWICE, the second time on the
    /// slot the arm's move-out had cap-zeroed:
    /// `match s.e { E.A(r) => println(r.id) }` printed `dR8 dE dR0` where one
    /// `dR8` is due. Raising `pattern_binding_is_borrow` fixes both halves at
    /// once — the whole suppression block (including
    /// `suppress_destructured_struct_field_enum_cleanup`, the memory half that
    /// did the zeroing) is gated on the flag being clear, and
    /// `bind_pattern_values` skips the binding's own registration.
    ///
    /// The ORDER this produces for a materializing arm (`let m = r;`) is the
    /// owner's — `dE` then the payload's body, at the OWNER's death — where the
    /// identifier spelling gives the arm the body first. Same count on both;
    /// the sequences differ because the owners differ, and design.md § Part 8
    /// fixes the order WITHIN a walk, not across two different values' deaths.
    ///
    /// THE OWNERSHIP WITNESS IS A LIVE BODIES WALK ON THE ROOT, not the shape.
    /// Standing the arm down when nothing else runs the body loses it outright —
    /// the same failure `place_walk_is_retractable` records from the other side
    /// for a fresh-temp scrutinee — so this asks the cleanup frames directly.
    /// An ESCAPING arm keeps today's transfer path (`no_arm_payload_escapes`
    /// defaults to "escapes" for any shape it does not recognise), because a
    /// binding that outlives the owner cannot be a view of it.
    fn scrutinee_is_readonly_owned_enum_projection(
        &self,
        scrutinee: &Expr,
        arms: &[MatchArm],
    ) -> bool {
        if !matches!(
            &scrutinee.kind,
            ExprKind::FieldAccess { .. } | ExprKind::TupleIndex { .. }
        ) {
            return false;
        }
        let Some(enum_name) = self.place_chain_type_name(scrutinee) else {
            return false;
        };
        let Some(layout) = self.type_decls.enum_layouts.get(enum_name.as_str()) else {
            return false;
        };
        // A `shared` enum drops through refcounts, not through the owner's
        // walk — the same carve-out the identifier classifier makes.
        if layout.is_shared {
            return false;
        }
        // Only the arms that would actually take a body-running payload are at
        // stake; a match that binds nothing the owner's walk would fire for is
        // not the shape that doubles, and there is no reason to move it off its
        // current path.
        if !arms
            .iter()
            .any(|a| self.enum_pattern_consumes_user_drop_payload(&enum_name, &a.pattern))
        {
            return false;
        }
        let Some(root) = self.place_expr_root_ident(scrutinee) else {
            return false;
        };
        let root_name = match &root.kind {
            ExprKind::Identifier(n) => n.clone(),
            ExprKind::SelfValue => "self".to_string(),
            _ => return false,
        };
        if !self.var_has_live_bodies_walk(&root_name) {
            return false;
        }
        self.no_arm_payload_escapes(arms)
    }

    /// Does a bodies-only user-`Drop` walk on `name` survive in a live scope
    /// frame? The ownership witness for
    /// [`Self::scrutinee_is_readonly_owned_enum_projection`] — a struct's
    /// `StructFieldBodies` walk or a tuple / container's `ContainerElemBodies`
    /// one, either of which reaches a projected enum's payload.
    ///
    /// `OwnWrapper` deliberately does NOT count: it is the root's own `impl
    /// Drop` body (or memory glue), which says nothing about whether a nested
    /// payload's body will run.
    fn var_has_live_bodies_walk(&self, name: &str) -> bool {
        self.drop_rc
            .scope_cleanup_actions
            .iter()
            .flatten()
            .any(|a| {
                matches!(
                    a,
                    crate::codegen::state::CleanupAction::UserDrop { binding_name, kind, .. }
                        if binding_name == name
                            && matches!(
                                kind,
                                crate::codegen::state::UserDropKind::StructFieldBodies
                                    | crate::codegen::state::UserDropKind::ContainerElemBodies
                            )
                )
            })
    }

    /// The block-body sibling of
    /// [`Self::scrutinee_is_readonly_owned_enum_local`], for the `if let`
    /// `then_block` / `while let` `body` scopes — the user-enum peer of
    /// `scrutinee_is_readonly_inline_optres_local_block`.
    ///
    /// B-2026-08-08-25 leg 3 reaches this site too: `control_flow.rs` calls the
    /// very same `suppress_destructured_enum_payload_cleanup`, so `if let
    /// E.A(v) = e { println(v); }` twice over a live local emptied the source
    /// exactly as the `match` spelling did — while the `Option`/`Result`
    /// spellings were already clean here, because only THEY had a block
    /// classifier. Fixing the match form alone would have left this measured
    /// hole under a closed row.
    pub(super) fn scrutinee_is_readonly_owned_enum_local_block(
        &self,
        scrutinee: &Expr,
        pattern: &Pattern,
        block: &crate::ast::Block,
    ) -> bool {
        let Some(enum_name) = self.owned_enum_local_that_frees_its_own_payload(scrutinee) else {
            return false;
        };
        if !self.enum_pattern_binds_heap_payload(&enum_name, pattern) {
            return false;
        }
        !self.pattern_bindings_escape_in_block(pattern, block)
    }

    /// The block-body sibling of
    /// [`Self::scrutinee_is_readonly_owned_enum_projection`], for the `if let`
    /// `then_block` / `while let` `body` scopes.
    ///
    /// B-2026-08-28-67's rule that the two spellings must answer the SAME
    /// question applies here too, and the `match`-only first cut of
    /// B-2026-08-29-29 broke it: `if let E.A(r) = s.e { println(r.id) }` still
    /// printed the payload's body twice while the `match` spelling of the same
    /// read printed it once. Same gates as the `match` form; only the escape
    /// test changes shape, from "no arm's binding escapes" to "this pattern's
    /// bindings do not escape this block".
    pub(super) fn scrutinee_is_readonly_owned_enum_projection_block(
        &self,
        scrutinee: &Expr,
        pattern: &Pattern,
        block: &crate::ast::Block,
    ) -> bool {
        if !matches!(
            &scrutinee.kind,
            ExprKind::FieldAccess { .. } | ExprKind::TupleIndex { .. }
        ) {
            return false;
        }
        let Some(enum_name) = self.place_chain_type_name(scrutinee) else {
            return false;
        };
        let Some(layout) = self.type_decls.enum_layouts.get(enum_name.as_str()) else {
            return false;
        };
        if layout.is_shared {
            return false;
        }
        if !self.enum_pattern_consumes_user_drop_payload(&enum_name, pattern) {
            return false;
        }
        let Some(root) = self.place_expr_root_ident(scrutinee) else {
            return false;
        };
        let root_name = match &root.kind {
            ExprKind::Identifier(n) => n.clone(),
            ExprKind::SelfValue => "self".to_string(),
            _ => return false,
        };
        if !self.var_has_live_bodies_walk(&root_name) {
            return false;
        }
        !self.pattern_bindings_escape_in_block(pattern, block)
    }

    /// Shared gate of the two user-enum caller-retains classifiers: is
    /// `scrutinee` a bare local holding a non-shared user enum that will
    /// PROVABLY free its own payload at scope exit? Returns the enum's name.
    ///
    /// Identifier only — deliberately NOT the owned-`self` receiver. For
    /// `impl E { fn into_id(self) { match self { … } } }` the source's
    /// scope-exit drop is the documented owned-self RESIDUAL: it does not fire,
    /// because the caller disarmed the value's payload-bodies walk at the call
    /// on the premise that the callee's arm takes ownership (B-2026-08-01-7).
    /// The arm is therefore the payload's sole owner, and classifying it as a
    /// borrow drops that owner outright — measured as a `Drop` body that
    /// stopped running (`e2e_owned_self_enum_receiver_single_fire`). An owned
    /// enum PARAM is not affected: its entry-registered `EnumDrop` really does
    /// fire, so the source is a genuine owner and one free is kept.
    ///
    /// The live-`EnumDrop` requirement is the ownership witness, not an
    /// inference from shape: suppressing the binding's own registration leaks
    /// unless the source really frees it. `track_enum_var` declines to register
    /// one for a shared enum and for an enum with no heap-bearing field
    /// anywhere — precisely the population where a borrow classification would
    /// have nobody to fall back on.
    fn owned_enum_local_that_frees_its_own_payload(&self, scrutinee: &Expr) -> Option<String> {
        let ExprKind::Identifier(scrut_name) = &scrutinee.kind else {
            return None;
        };
        let slot = self.variables.get(scrut_name.as_str())?;
        let enum_name = self
            .var_types
            .var_type_names
            .get(scrut_name.as_str())
            .cloned()?;
        let layout = self.type_decls.enum_layouts.get(enum_name.as_str())?;
        if layout.is_shared {
            return None;
        }
        if !self.slot_has_live_enum_drop(slot.ptr) {
            return None;
        }
        Some(enum_name)
    }

    /// Does a `CleanupAction::EnumDrop` on `slot_ptr` survive in any live scope
    /// frame? The ownership witness for
    /// [`Self::owned_enum_local_that_frees_its_own_payload`].
    fn slot_has_live_enum_drop(&self, slot_ptr: PointerValue<'ctx>) -> bool {
        self.drop_rc
            .scope_cleanup_actions
            .iter()
            .flatten()
            .any(|a| {
                matches!(
                    a,
                    crate::codegen::state::CleanupAction::EnumDrop { enum_alloca, .. }
                        if *enum_alloca == slot_ptr
                )
            })
    }

    /// Does `pattern` bind at least one HEAP-BEARING payload field of
    /// `enum_name` — i.e. a field
    /// [`Self::suppress_destructured_enum_payload_cleanup_at`] would zero in the
    /// source? Same `(variant, consumed positions)` walk and same
    /// `is_heap_bearing` filter, so the two agree by construction.
    fn enum_pattern_binds_heap_payload(&self, enum_name: &str, pattern: &Pattern) -> bool {
        let Some(layout) = self.type_decls.enum_layouts.get(enum_name) else {
            return false;
        };
        let Some((variant_name, consumed_positions)) =
            self.enum_pattern_consumed_positions(enum_name, pattern)
        else {
            return false;
        };
        let Some(drop_kinds) = layout.field_drop_kinds.get(&variant_name) else {
            return false;
        };
        consumed_positions
            .iter()
            .any(|&pos| drop_kinds.get(pos).is_some_and(|k| k.is_heap_bearing()))
    }

    /// B-2026-08-08-25 — the caller-retains classifier for a LIVE LOCAL owning
    /// an inline `Option`/`Result` `{ptr,len,cap}` payload, when no arm moves
    /// that payload out.
    ///
    /// `match o { Some(v) => println(v) }` over a live `let o: Option[String]`
    /// used to hand the buffer to the arm binding — which `track_vec_var`s it
    /// and frees it at arm exit — while zeroing the source's `cap` so the
    /// source's own free skipped. `o` stays in scope and readable, so every
    /// later read was a use-after-free: valgrind reports an invalid read of the
    /// freed block, and a second `match o` printed garbage bytes or aborted in
    /// the UTF-8 validator.
    ///
    /// The aggregate payload has been consumption-gated since B-2026-07-03-31
    /// (`arm_only_borrows_option_agg_payload`); that gate was deliberately
    /// narrowed to the tuple, leaving the DIRECT `String`/`Vec` payload with an
    /// unconditional suppression. This is the missing direct-heap sibling, and
    /// it takes the same shape as `scrutinee_is_readonly_owned_agg_loop_var`
    /// above: a read-only match over an owned source is the BORROW path.
    ///
    /// Raising `pattern_binding_is_borrow` for it does both halves at once —
    /// `bind_pattern_values` skips the arm binding's `track_vec_var` (nothing
    /// frees at arm exit) and the source's `FreeInlineOptionPayload` stays
    /// armed to free it once at scope exit. Zero runtime cost: no clone, one
    /// free, and the source stays valid, which is what `--interp` already did.
    ///
    /// Conservative in the safe direction. `no_arm_payload_escapes` defaults to
    /// "escapes" for any shape it does not recognise, so a wrong answer keeps
    /// today's transfer rather than inventing a double-free; and if an arm does
    /// move the binding after all, the borrow path's existing
    /// `clone_escaping_borrow_payload_binding` leg deep-clones it.
    fn scrutinee_is_readonly_inline_optres_local(
        &self,
        scrutinee: &Expr,
        arms: &[MatchArm],
    ) -> bool {
        let optres_te = self.scrutinee_optres_type_expr(scrutinee);
        let destructures = arms
            .iter()
            .any(|a| self.pattern_destructures_heap_payload(optres_te.as_ref(), &a.pattern));
        // B-2026-08-30-52 (b) — the second shape that needs the BOXED channel.
        // `Some(p) => match p { … }` binds the payload WHOLE, so `destructures`
        // is false and a boxed payload (`Option[E]`, whose 4 words overflow
        // Option's 3-word inline area) never reached the classifier at all —
        // which is why the `Result` twin of the same program, inline at 5 words,
        // behaved differently on an axis the source does not mention.
        //
        // Admitted under a STRICTER use test than `destructures` gets, because a
        // whole binding over a box is exactly the shape the row measured as
        // unsafe to admit wholesale: `binding_only_nested_match_scrutinee`
        // rejects a bare use (moved on) AND a projection (field-moved-out), the
        // two classes that broke those five tests, while a nested `match` takes
        // nothing by itself — whether it does is decided by
        // `no_arm_payload_escapes` below.
        let nested_match_only = arms
            .iter()
            .any(|a| self.arm_payload_binding_only_feeds_nested_match(&a.pattern, &a.body))
            && arms.iter().all(|a| {
                !Self::pattern_consumes_variant_payload(&a.pattern)
                    || self.pattern_destructures_heap_payload(optres_te.as_ref(), &a.pattern)
                    || self.arm_payload_binding_only_feeds_nested_match(&a.pattern, &a.body)
                    || optres_te.as_ref().is_some_and(|te| {
                        Self::pattern_binds_scalar_variant_payload(te, &a.pattern)
                    })
            });
        if !(self.scrutinee_is_inline_optres_local(scrutinee)
            || ((destructures || nested_match_only)
                && self.scrutinee_is_boxed_optres_local(scrutinee)))
        {
            return false;
        }
        if !arms
            .iter()
            .any(|a| self.pattern_binds_direct_inline_heap_payload(&a.pattern))
            && !destructures
        {
            return false;
        }
        // Every payload-consuming arm must be the direct-heap shape, the
        // destructured-heap shape, or a payload that owns nothing — a mixed
        // match must keep today's behavior wholesale, since the flag is set
        // once for the whole `match`.
        if !arms.iter().all(|a| {
            !Self::pattern_consumes_variant_payload(&a.pattern)
                || self.pattern_binds_direct_inline_heap_payload(&a.pattern)
                || self.pattern_destructures_heap_payload(optres_te.as_ref(), &a.pattern)
                || optres_te
                    .as_ref()
                    .is_some_and(|te| Self::pattern_binds_scalar_variant_payload(te, &a.pattern))
        }) {
            return false;
        }
        self.no_arm_payload_escapes(arms)
    }

    /// Shared membership half of the two caller-retains classifiers: is
    /// `scrutinee` a bare local that owns an `Option`/`Result` payload?
    ///
    /// ALL FOUR REPRESENTATIONS, not just the inline one (B-2026-08-30-47).
    /// An `Option`/`Result` payload is owned through one of several parallel
    /// channels, each with its own registry and its own scope-exit action:
    ///
    ///   `inline_{option,result}_payload_vars`  `String`/`Vec` overlaid on the
    ///                                          payload words
    ///   `inline_option_map_payload_vars`       a `Map`/`Set` handle
    ///   `inline_option_agg_payload_vars`       a user struct/enum the
    ///                                          recursive drop family frees
    ///
    /// B-2026-08-08-25 built the read-only-arm borrow classification on the
    /// first channel alone, and B-2026-08-30-19 widened what counts as a heap
    /// payload WITHIN it. Neither reached the other two, so a read-only arm
    /// over `Option[Map]` / `Option[Set]` / `Option[SortedMap]` /
    /// `Option[E]` / `Option[Option[String]]` still TOOK the payload: measured
    /// as the source reading back `None` on the `Map` family, an empty print
    /// for `Result[E]`, and a double-free abort for the two enum shapes.
    ///
    /// Membership is the right question to widen because each channel already
    /// registers its own payload correctly — the defect was never a missing
    /// registration, it was this predicate knowing about two registries out of
    /// four. The disarm sites are the other half: each channel's suppressor
    /// must honour `pattern_binding_source_retains_inline_payload` the way the
    /// inline one has since B-2026-08-08-25, or the classification decides
    /// "borrow" and the source is disarmed anyway.
    fn scrutinee_is_inline_optres_local(&self, scrutinee: &Expr) -> bool {
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return false;
        };
        // A passthrough binding owns nothing — ask about its source, exactly as
        // the suppressors themselves forward (B-2026-08-06-27).
        let name: &str = self
            .payload_vars
            .passthrough_owner_alias
            .get(name.as_str())
            .map_or(name.as_str(), |s| s.as_str());
        self.payload_vars.inline_option_payload_vars.contains(name)
            || self.payload_vars.inline_result_payload_vars.contains(name)
            || self
                .payload_vars
                .inline_option_map_payload_vars
                .contains(name)
            || self
                .payload_vars
                .inline_option_agg_payload_vars
                .contains(name)
    }

    /// The HEAP-BOXED payload channel (`boxed_enum_payload_vars`), held apart
    /// from [`Self::scrutinee_is_inline_optres_local`] deliberately.
    ///
    /// B-2026-08-30-52 — a wide payload (a multi-field struct, a tuple, a user
    /// enum) does not fit the payload words and is boxed, so it is the channel
    /// most of the destructuring shapes actually ride. But a box is a different
    /// ownership regime from an inline payload, and admitting it to the
    /// read-only classification wholesale is NOT safe: measured, it broke five
    /// tests, every one of them a boxed payload bound WHOLE and then moved on
    /// or field-moved-out (`e2e_named_optres_boxed_payload_arm_runs_user_drop_
    /// body`, `test_e2e_user_drop_scrutinee_option_field_move_out_runs`, and
    /// three ASAN cases including `asan_consuming_arm_boxed_payload_handed_on_
    /// is_freed_once`).
    ///
    /// So it is admitted only together with a DESTRUCTURING pattern, which is
    /// the shape this row is about and the one where the leaves' owners are
    /// what would double-free. A whole-payload binding over a boxed local keeps
    /// the path it takes today.
    fn scrutinee_is_boxed_optres_local(&self, scrutinee: &Expr) -> bool {
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return false;
        };
        let name: &str = self
            .payload_vars
            .passthrough_owner_alias
            .get(name.as_str())
            .map_or(name.as_str(), |s| s.as_str());
        self.payload_vars.boxed_enum_payload_vars.contains(name)
    }

    /// Does `pattern` bind a `Some`/`Ok`/`Err` payload out at all? (Shape only —
    /// says nothing about what the payload is.)
    fn pattern_consumes_variant_payload(pattern: &Pattern) -> bool {
        let PatternKind::TupleVariant { path, patterns } = &pattern.kind else {
            return false;
        };
        matches!(
            path.last().map(|s| s.as_str()),
            Some("Some") | Some("Ok") | Some("Err")
        ) && patterns.iter().any(pattern_consumes_field)
    }

    /// B-2026-08-30-52 — the payload bound by a DESTRUCTURING pattern
    /// (`Some(S { s })`, `Some((s, n))`) rather than whole.
    ///
    /// [`Self::pattern_binds_direct_inline_heap_payload`] requires every
    /// payload-consuming sub-pattern to be a plain `Binding`, so a
    /// destructuring pattern fails it BY CONSTRUCTION, the arm is never
    /// classified a borrow, and the transfer path runs: the destructured leaf
    /// registers as an owner and frees at arm exit while the source's drop is
    /// still armed. Measured on `main`: `Option[S { s: String }]` read back
    /// garbage and `Option[(String, i64)]` aborted with a double free, on every
    /// compiled surface, with `--interp` correct.
    ///
    /// JUDGED FROM THE SCRUTINEE'S INSTANTIATION, not from the leaves. A
    /// shorthand struct field (`S { s }`) carries NO sub-`Pattern` at all — the
    /// field name IS the binding — so there is no span to look up in
    /// `pattern_binding_types` and no per-leaf type to read. The payload's own
    /// `TypeExpr` is a positive witness that needs neither.
    ///
    /// Requiring the payload to OWN HEAP keeps this confined to the shapes that
    /// are broken: a payload that owns nothing already takes its path
    /// correctly, and classifying it as a borrow would move it for no reason.
    /// `field_te_owns_heap` answers `false` for a `shared` payload, so the RC
    /// regime stays out exactly as it does for the whole-binding classifier.
    ///
    /// This does NOT relax the whole-binding test — a `Binding` sub-pattern is
    /// rejected here and left to that classifier. The distinction matters:
    /// admitting a WHOLE-bound tuple payload as a borrow is what emptied
    /// `e2e_optres_tuple_payload_is_owned_exactly_once` in B-2026-08-08-25 and
    /// again in B-2026-08-30-47's first attempt. What is admitted here is the
    /// destructured spelling, whose leaves skip their own `track_vec_var` under
    /// `pattern_binding_is_borrow` — `bind_pattern_values` gates every owner
    /// registration on that flag and recurses, so the leaves come out borrowed
    /// without a second mechanism.
    fn pattern_destructures_heap_payload(
        &self,
        optres_te: Option<&TypeExpr>,
        pattern: &Pattern,
    ) -> bool {
        let PatternKind::TupleVariant { path, patterns } = &pattern.kind else {
            return false;
        };
        let arg_idx = match path.last().map(|s| s.as_str()) {
            Some("Some") | Some("Ok") => 0,
            Some("Err") => 1,
            _ => return false,
        };
        if patterns.len() != 1 {
            return false;
        }
        let sub = &patterns[0];
        if !pattern_consumes_field(sub) {
            return false;
        }
        // A whole-payload binding belongs to the sibling classifier; only the
        // DESTRUCTURING spellings are this one's business.
        if !matches!(
            &sub.kind,
            PatternKind::Struct { .. } | PatternKind::Tuple(_)
        ) {
            return false;
        }
        let Some(te) = optres_te else {
            return false;
        };
        let TypeKind::Path(p) = &te.kind else {
            return false;
        };
        let Some(GenericArg::Type(arg)) = p.generic_args.as_ref().and_then(|a| a.get(arg_idx))
        else {
            return false;
        };
        self.field_te_owns_heap(arg, 8)
    }

    /// B-2026-08-08-25 — restricts the caller-retains classifiers to the DIRECT
    /// `{ptr,len,cap}` payload (`Option[String]`, `Option[Vec[i64]]`), bound
    /// WHOLE.
    ///
    /// `inline_{option,result}_payload_vars` is not a payload-shape witness: it
    /// also carries TUPLE payloads, registered at their let-site by
    /// B-2026-08-05-3. Those already have their own consumption gate
    /// (`arm_only_borrows_result_tuple_payload`), and their bindings own
    /// themselves — a destructuring `Some((v, k))` gives each heap element its
    /// own `track_vec_var`. Classifying them as borrows dropped those owners and
    /// broke `e2e_optres_tuple_payload_is_owned_exactly_once` (the program
    /// produced no output at all), which is what surfaced this gate.
    ///
    /// `pattern_binding_types` records the bound payload's type name, so it
    /// separates the two directly — the same witness `result_tuple_payload_binds`
    /// uses to recognise its own shape from the other side.
    fn pattern_binds_direct_inline_heap_payload(&self, pattern: &Pattern) -> bool {
        let PatternKind::TupleVariant { path, patterns } = &pattern.kind else {
            return false;
        };
        if !matches!(
            path.last().map(|s| s.as_str()),
            Some("Some") | Some("Ok") | Some("Err")
        ) {
            return false;
        }
        if !patterns.iter().any(pattern_consumes_field) {
            return false;
        }
        patterns
            .iter()
            .filter(|p| pattern_consumes_field(p))
            .all(|p| {
                matches!(&p.kind, PatternKind::Binding(_))
                    && self
                        .pattern_state
                        .pattern_binding_types
                        .get(&(p.span.offset, p.span.length))
                        .is_some_and(|t| self.binding_type_name_owns_heap(t, 8))
            })
    }

    /// Does the user struct `name` own heap, transitively through its fields?
    ///
    /// B-2026-08-30-19 — the classification above admitted only a DIRECT
    /// `{ptr,len,cap}` payload, so `Option[S]` where `S` merely CONTAINS a
    /// `String` was not a borrow: the read-only arm took the payload, the source
    /// was left dangling, and a second read was a use-after-free that printed
    /// garbage on the JIT and both AOT legs while `--interp` was correct. The
    /// same struct inside a USER enum variant was already right, so this is the
    /// built-in `Option`/`Result` channel specifically.
    ///
    /// TRANSITIVE, not one hop: `Outer { i: Inner { s: String } }` fails exactly
    /// the same way and a one-hop check would still miss it.
    ///
    /// A struct with no heap below it stays out, which keeps
    /// `Option[S { n: i64 }]` on the path it already takes correctly — the
    /// change is confined to the shapes that are broken today.
    ///
    /// `fuel` bounds a cycle: `struct A { b: Option[B] }` /
    /// `struct B { a: Option[A] }` is representable, and running out answers
    /// "owns heap", the conservative direction (a borrow, so the source keeps
    /// its payload — the failure mode of being wrong here is a missed
    /// optimization, not a dangling read).
    fn named_struct_owns_heap(&self, name: &str, fuel: u32) -> bool {
        if fuel == 0 {
            return true;
        }
        // A `shared struct` is NOT admitted. Its payload is an RC box, a
        // different ownership regime from the value payloads B-2026-08-08-25's
        // borrow model is about: a field read off one is DEEP-CLONED
        // (B-2026-08-13-6) precisely because the box may have other handles, so
        // classifying the arm as a borrow drops an owner the RC path still
        // expects. Measured: admitting them turned
        // `asan_shared_struct_heap_field_read_cloned_not_aliased` — whose
        // program matches an `Option[Inner]` over a `shared struct Inner` — into
        // an ASAN memory error, while every value-struct shape stayed correct.
        if self.type_decls.shared_type_decl_names.contains(name)
            || self.type_decls.shared_type_names.contains(name)
        {
            return false;
        }
        let Some(ftes) = self.type_decls.struct_field_type_exprs.get(name) else {
            return false;
        };
        ftes.iter()
            .any(|fte| self.field_te_owns_heap(fte, fuel - 1))
    }

    /// Does a payload BINDING whose recorded surface type name is `name` own
    /// heap that a read-only arm must not take?
    ///
    /// B-2026-08-30-47, PARTIAL — the entry point of
    /// [`Self::named_struct_owns_heap`] was struct-only, while
    /// [`Self::field_te_owns_heap`] one level DOWN was already conservative
    /// ("anything not a scalar and not a known heap-free struct owns heap").
    /// A payload the walker would have called heap-owning as a FIELD was
    /// called heap-free as the payload ITSELF, so the arm took it.
    ///
    /// READ THE ROW BEFORE EXTENDING THIS. Seven shapes failed four ways —
    /// `Option[Map]`/`Set`/`SortedMap` read back as the `None` arm,
    /// `Result[E]` printed empty, `Option[E]` and `Option[Option[String]]`
    /// aborted with a double free, and `Result[Map]` segfaulted. Closing this
    /// asymmetry fixes exactly ONE of them (`Result[Map]`, segfault to
    /// correct). The other six never reach this classifier at all: the gate in
    /// front of it requires membership in `inline_{option,result}_payload_vars`,
    /// and `track_inline_option_payload_var` registers nothing unless the
    /// payload is an inline heap `Vec`/`String`. That is the load-bearing half
    /// and it is NOT fixed here.
    ///
    /// This half is landed on its own terms: two ends of one judgement that
    /// disagree are a latent defect whatever they currently reach. Six control
    /// shapes that were already correct — including `Option[shared struct]`
    /// and a payload-free enum — stay correct.
    ///
    /// Judging the NAME rather than the payload's `TypeExpr` is deliberate:
    /// `pattern_binding_types` is what the classifier has in hand at this
    /// point, and it records the head name for every named payload
    /// (`Map`, `Set`, a user enum, `Option`). A generic ARGUMENT cannot change
    /// the answer for any name reached here, because every container in the
    /// `else` arm owns its buffer whatever it holds.
    fn binding_type_name_owns_heap(&self, name: &str, fuel: u32) -> bool {
        // A raw pointer has no owner to drop.
        if name.starts_with('*') || is_scalar_surface_name(name) {
            return false;
        }
        // A WHOLE-TUPLE binding is recorded under the literal name "Tuple",
        // and it must stay out. Tuple payloads have their own consumption gate
        // (`arm_only_borrows_result_tuple_payload`) and their bindings own
        // THEMSELVES — each heap element gets its own `track_vec_var` — so
        // classifying the arm as a borrow drops those owners. That is the
        // exact hazard `pattern_binds_direct_inline_heap_payload`'s own doc
        // describes, and it is not hypothetical: an earlier version of this
        // function fell through to a conservative default here and turned
        // `e2e_optres_tuple_payload_is_owned_exactly_once` into a program that
        // produced NO OUTPUT AT ALL — the same signature B-2026-08-08-25
        // recorded when it first drew this line.
        if name == "Tuple" {
            return false;
        }
        if let Some(known) = self.named_type_owns_heap(name, fuel) {
            return known;
        }
        // An ALLOW-LIST, not a conservative default, and the tuple regression
        // above is why. "Conservative" here does NOT mean "answer true when
        // unsure": answering true classifies the arm as a borrow, which DROPS
        // an owner whenever the payload turns out to own itself. Both answers
        // are unsafe for some shape, so this admits only names measured to
        // need it and leaves every unresolved name on the path it takes today.
        matches!(
            name,
            "String" | "Vec" | "Map" | "Set" | "SortedMap" | "SortedSet"
        )
    }

    /// The owns-heap answer for a name this backend has a DECLARATION for, or
    /// `None` when it has none. A `shared struct` answers `false`: its payload
    /// is an RC box, a different ownership regime — a field read off one is
    /// deep-cloned (B-2026-08-13-6) because the box may have other handles, so
    /// classifying the arm as a borrow drops an owner the RC path still
    /// expects. Measured: admitting them turns
    /// `asan_shared_struct_heap_field_read_cloned_not_aliased` into an ASAN
    /// memory error.
    fn named_type_owns_heap(&self, name: &str, fuel: u32) -> Option<bool> {
        if self.type_decls.shared_type_decl_names.contains(name)
            || self.type_decls.shared_type_names.contains(name)
        {
            return Some(false);
        }
        if self.type_decls.struct_field_type_exprs.contains_key(name) {
            return Some(self.named_struct_owns_heap(name, fuel));
        }
        let variants = self.enum_variant_field_type_exprs(name);
        if !variants.is_empty() {
            return Some(self.named_enum_owns_heap(&variants, fuel));
        }
        None
    }

    /// The enum sibling of [`Self::named_struct_owns_heap`]: an enum owns heap
    /// when ANY variant carries a payload that does. This is what keeps
    /// `Option[F]` for a payload-free or all-scalar `enum F` OUT — a control
    /// that is correct today and that a blanket "an enum owns heap" rule would
    /// have moved off the path it takes correctly.
    fn named_enum_owns_heap(&self, variants: &[(u64, String, Vec<TypeExpr>)], fuel: u32) -> bool {
        if fuel == 0 {
            return true;
        }
        variants
            .iter()
            .any(|(_, _, tes)| tes.iter().any(|te| self.field_te_owns_heap(te, fuel - 1)))
    }

    /// The field-type half of [`Self::named_struct_owns_heap`]. A scalar owns
    /// nothing; a known struct recurses; everything else (`String`, `Vec`,
    /// `Map`, an enum, an unresolved name) counts as owning, matching
    /// `te_owns_heap_below_buffer`'s conservative default.
    fn field_te_owns_heap(&self, te: &crate::ast::TypeExpr, fuel: u32) -> bool {
        match &te.kind {
            crate::ast::TypeKind::Tuple(elems) => {
                elems.iter().any(|e| self.field_te_owns_heap(e, fuel))
            }
            crate::ast::TypeKind::Path(p) => {
                let head = p.segments.last().map(String::as_str).unwrap_or("");
                if is_scalar_surface_name(head) {
                    return false;
                }
                // Resolves a user ENUM as well as a struct (B-2026-08-30-47).
                // Before that, an enum field fell to the `true` below, so a
                // struct holding a payload-free enum counted as owning heap —
                // conservative, and only a missed optimization here, but the
                // SAME gap at the binding entry point was a use-after-free.
                self.named_type_owns_heap(head, fuel).unwrap_or(true)
            }
            _ => true,
        }
    }

    /// The resolved `Option[P]` / `Result[O, E]` instantiation of a bare-local
    /// scrutinee, by the same two-table lookup and alias resolution the
    /// consuming-arm clone uses (`clone_escaping_live_local_option`).
    fn scrutinee_optres_type_expr(&self, scrutinee: &Expr) -> Option<TypeExpr> {
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return None;
        };
        let owner: &str = self
            .payload_vars
            .passthrough_owner_alias
            .get(name.as_str())
            .map_or(name.as_str(), |s| s.as_str());
        self.type_decls
            .enum_inst_var_types
            .get(owner)
            .or_else(|| self.var_types.optres_var_payload_tes.get(owner))
            .cloned()
    }

    /// B-2026-08-08-25 leg 2 — does `pattern` bind a variant payload that OWNS
    /// NOTHING, judged from the scrutinee's own instantiation?
    ///
    /// The sibling gate above disqualifies a whole `match` when any
    /// payload-consuming arm is not the direct-heap shape, because the borrow
    /// flag is set once for the entire match and a TUPLE payload's bindings own
    /// themselves (each heap element gets its own `track_vec_var`) — classifying
    /// those as borrows drops their owners.
    ///
    /// A SCALAR payload has no owner to drop, so that rationale does not reach
    /// it, and disqualifying on it cost the whole classifier: `Result[String,
    /// i64]` matched read-only twice printed `hi` and then an EMPTY line,
    /// because `Err(e)`'s `i64` binding failed the direct-heap test and sent the
    /// `Ok` arm back to the transfer path that empties the source. That is the
    /// entire reason this defect looked `Result`-specific and looked like a
    /// `.map` defect: `Option`'s only co-arm is `None`, which binds nothing and
    /// so can never trip the gate, and the read-only spelling fails identically.
    ///
    /// Keyed on the scrutinee's `TypeExpr`, NOT on `pattern_binding_types`,
    /// because that table is a NARROWING record: it stores `u8`/`i32`/`f64` and
    /// every named type, but a plain 64-bit int is the default width and gets NO
    /// entry at all. An absent entry therefore cannot witness scalar-ness.
    /// Reading the variant's argument off the instantiation is a POSITIVE
    /// witness instead.
    ///
    /// CORRECTION (B-2026-08-30-47, measured): this passage used to add that an
    /// absent entry "also covers a whole-tuple binding (`Some(t)` over
    /// `Option[(String, i64)]`)". It does not — `bind_pattern_types` records a
    /// `Type::Tuple` binding under the literal name `"Tuple"`
    /// (`typechecker/patterns.rs`), so the entry is PRESENT and it is that name,
    /// not its absence, that keeps such a binding out of the heap-payload
    /// classifier. The conclusion above is unaffected; the supporting claim was
    /// wrong, and believing it cost a regression — `binding_type_name_owns_heap`
    /// was first written with a conservative "unknown name owns heap" default on
    /// the reasoning that a tuple could not reach it, which admitted `"Tuple"`
    /// and emptied `e2e_optres_tuple_payload_is_owned_exactly_once`.
    ///
    /// Conservative in the safe direction on every axis: a single plain
    /// `Binding` only (so a destructuring `Some((v, k))` stays rejected), a
    /// bare single-segment primitive path only (so `Vec[i64]`, a user type, or
    /// anything generic keeps disqualifying the match), and no instantiation
    /// on file means no exemption.
    fn pattern_binds_scalar_variant_payload(optres_te: &TypeExpr, pattern: &Pattern) -> bool {
        let PatternKind::TupleVariant { path, patterns } = &pattern.kind else {
            return false;
        };
        let arg_idx = match path.last().map(|s| s.as_str()) {
            Some("Some") | Some("Ok") => 0,
            Some("Err") => 1,
            _ => return false,
        };
        if patterns.len() != 1 || !matches!(&patterns[0].kind, PatternKind::Binding(_)) {
            return false;
        }
        let TypeKind::Path(p) = &optres_te.kind else {
            return false;
        };
        let Some(GenericArg::Type(arg)) = p.generic_args.as_ref().and_then(|a| a.get(arg_idx))
        else {
            return false;
        };
        let TypeKind::Path(arg_path) = &arg.kind else {
            return false;
        };
        arg_path.generic_args.is_none()
            && arg_path.segments.len() == 1
            && crate::codegen::param_own::is_primitive_type_name(&arg_path.segments[0])
    }

    /// Block-body sibling of
    /// [`Self::scrutinee_is_readonly_inline_optres_local`] for the if-let
    /// `then_block` / while-let `body` scopes.
    ///
    /// Deliberately NOT used at the `let…else` site: that binding escapes into
    /// the enclosing scope by construction, so there is no block whose reads
    /// bound its lifetime and the unconditional suppression there is correct.
    pub(super) fn scrutinee_is_readonly_inline_optres_local_block(
        &self,
        scrutinee: &Expr,
        pattern: &Pattern,
        block: &crate::ast::Block,
    ) -> bool {
        let destructures = self.pattern_destructures_heap_payload(
            self.scrutinee_optres_type_expr(scrutinee).as_ref(),
            pattern,
        );
        // B-2026-08-30-52 (b) — the `if let` copy of the `match` path's boxed
        // whole-binding admission. B-2026-08-28-67's rule is that the two
        // spellings must answer the SAME question, and a `match`-only fix would
        // have left `if let Some(p) = a { match p { … } }` broken under a row
        // whose other spelling reads correctly.
        let nested_match_only =
            self.pattern_payload_binding_only_feeds_nested_match_block(pattern, block);
        if !(self.scrutinee_is_inline_optres_local(scrutinee)
            || ((destructures || nested_match_only)
                && self.scrutinee_is_boxed_optres_local(scrutinee)))
        {
            return false;
        }
        // B-2026-08-30-52 — the `if let` copy of the `match` path's second
        // disjunct. Without it `if let Some(S { s }) = a { … }` stayed on the
        // transfer path while the identical `match` spelling was a borrow, so
        // the same program read back garbage through one keyword and printed
        // correctly through the other.
        if !self.pattern_binds_direct_inline_heap_payload(pattern) && !destructures {
            return false;
        }
        !self.pattern_bindings_escape_in_block(pattern, block)
    }

    /// `for p in items { match p { A(x) => <MOVE x> … } }` where `items: Vec[E]`
    /// is a heap-bearing non-shared user ENUM: the loop element `p` is a bit-copy
    /// alias of the container's slot (registered in `for_loop_owned_agg_vars`
    /// under the deep-copy-on-whole-move model). A match arm that MOVES a payload
    /// out of `p` raw-moves it off that alias, so the consumer frees the buffer
    /// AND the container's per-element drop frees it again → double-free
    /// (B-2026-07-14-1; the f-string `render` loop in the self-hosted lexer). The
    /// whole-move deep-copy model (`deep_copy_for_loop_agg_element_move`) only
    /// fires on a `let x = p` / struct-literal whole-element move, NOT on a match
    /// arm's PARTIAL payload move, so the escape falls through uncovered — the
    /// gap `scrutinee_is_readonly_owned_agg_loop_var` explicitly punts to "the
    /// owned path", whose source-cap suppression is a no-op against a bit-copy
    /// alias. Deep-copy the element's payload into an INDEPENDENT buffer here
    /// (the loop-element sibling of `clone_owned_vec_index_element` for `v[i]`),
    /// so the arm extracts from the clone and the container's original element is
    /// freed exactly once. Returns `(value, did_clone)`; `did_clone` forces the
    /// clone through `materialize_freshtemp_enum_scrutinee` so a no-bind arm or
    /// an unbound heap field frees the clone (no leak), and each arm's per-field
    /// suppression zeroes the CLONE's moved-out caps. Gated to the ESCAPING case
    /// — a read-only match is already handled as a borrow by
    /// `scrutinee_is_readonly_owned_agg_loop_var`, and cloning there would leak.
    pub(super) fn clone_escaping_owned_agg_loop_var_enum(
        &mut self,
        scrutinee: &Expr,
        val: BasicValueEnum<'ctx>,
        arms: &[MatchArm],
    ) -> (BasicValueEnum<'ctx>, bool) {
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return (val, false);
        };
        if !self
            .borrow_vars
            .for_loop_owned_agg_vars
            .contains(name.as_str())
        {
            return (val, false);
        }
        // Only an ESCAPING payload needs the independent buffer; a read-only
        // match is the borrow path (drop already suppressed) — cloning would leak.
        if self.no_arm_payload_escapes(arms) {
            return (val, false);
        }
        let Some(enum_name) = self.type_name_of_expr(scrutinee) else {
            return (val, false);
        };
        let Some(layout) = self.type_decls.enum_layouts.get(&enum_name).cloned() else {
            return (val, false);
        };
        // Shared enums are RC-boxed — no value `EnumDrop` to race, leave untouched.
        if layout.is_shared {
            return (val, false);
        }
        let fn_val = self.current_fn.unwrap();
        let ll = val.get_type();
        let slot = self.create_entry_alloca(fn_val, "loopelem.enum.clone", ll);
        self.builder.build_store(slot, val).unwrap();
        // In-place payload deep-copy — the same duplication the `let x = p`
        // whole-move path uses (copy-depth == drop-depth), so `slot` now owns an
        // independent copy of exactly the payloads `emit_enum_drop_switch` frees.
        self.deep_copy_enum_heap_payload_in_place(&enum_name, slot, &layout);
        (
            self.builder
                .build_load(ll, slot, "loopelem.enum.cloned")
                .unwrap(),
            true,
        )
    }

    /// B-2026-08-08-25 leg 3, consuming half — the LIVE-LOCAL sibling of
    /// [`Self::clone_escaping_owned_agg_loop_var_enum`], and the user-enum twin
    /// of the `Option`/`Result` live-local clones.
    ///
    /// `let e: E = E.A([f"aa", f"bb"]); match e { E.A(v) => { for s in v { … } }
    /// … }` followed by a re-read of `e` printed the payload once and then
    /// nothing: the `for` MOVES `v`, so the read-only classifier correctly
    /// declines and the transfer path zeroes the source's payload words —
    /// leaving the still-in-scope `e` reading an empty `Vec`. Deep-copy the
    /// enum so the consuming arm extracts from an independent buffer and the
    /// live source keeps its own.
    ///
    /// `deep_copy_enum_heap_payload_in_place` is the right duplicator here (and
    /// NOT the type-directed `emit_clone_fn_for_type_expr` the `Option` leg
    /// uses) because a user enum's payload rides `layout.field_drop_kinds`:
    /// copy-depth then equals drop-depth by construction, which is the property
    /// that keeps the two buffers independently freeable.
    ///
    /// Gated on liveness like its siblings — a source that is dead after the
    /// match has exactly one owner already and keeps today's zero-cost
    /// transfer, which is what keeps this off the common consuming shape.
    pub(super) fn clone_escaping_live_local_enum(
        &mut self,
        scrutinee: &Expr,
        val: BasicValueEnum<'ctx>,
        arms: &[MatchArm],
    ) -> (BasicValueEnum<'ctx>, bool) {
        let patterns: Vec<&Pattern> = arms.iter().map(|a| &a.pattern).collect();
        let live =
            |cg: &Self, name: &str| cg.scrutinee_read_after_match(name, name, scrutinee, arms);
        self.clone_escaping_live_local_enum_core(
            scrutinee,
            val,
            &patterns,
            !self.no_arm_payload_escapes(arms),
            &live,
        )
    }

    /// B-2026-08-09-11 — the BLOCK-form sibling of
    /// [`Self::clone_escaping_live_local_enum`], for `if let` / `while let`.
    ///
    /// The `match` spelling of `let e = E.A(f"hi"); if let E.A(v) = e { let k:
    /// String = v; … } if let E.A(v) = e { … }` was already correct; the
    /// if-let spelling printed the payload once and then an EMPTY line,
    /// because only the match site carried the live-local clone leg. Same
    /// `did_clone` → `materialize_freshtemp_enum_scrutinee` contract as the
    /// ref-chain leg that already sits at these sites.
    ///
    /// Liveness bounds on the BLOCK rather than on arm bodies, which is the
    /// whole reason this is a second entry point rather than a shared call:
    /// without the gate the clone would fire on every consuming `if let`,
    /// including the common DEAD-source shape that is correct and free on
    /// today's transfer path.
    ///
    /// `escapes` is a parameter rather than computed here because the three
    /// callers derive it differently: `if let` / `while let` ask whether the
    /// pattern's bindings escape their block, while a `let…else` binding
    /// escapes into the ENCLOSING scope by construction and so passes an
    /// unconditional `true` — the same convention the ref-chain leg at that
    /// site already uses. `block` is correspondingly the construct's LAST
    /// block (the then-block, the loop body, or the else-block), whose end
    /// bounds the construct for the liveness gate.
    pub(super) fn clone_escaping_live_local_enum_block(
        &mut self,
        scrutinee: &Expr,
        val: BasicValueEnum<'ctx>,
        pattern: &Pattern,
        escapes: bool,
        block: &crate::ast::Block,
        else_end: Option<usize>,
    ) -> (BasicValueEnum<'ctx>, bool) {
        let live = |cg: &Self, name: &str| {
            cg.scrutinee_read_after_block(name, name, scrutinee, block, else_end)
        };
        self.clone_escaping_live_local_enum_core(scrutinee, val, &[pattern], escapes, &live)
    }

    /// Shared core of the two live-local enum clone entry points. `escapes`
    /// and `live` are the two gates whose computation differs between the
    /// arm-list and block forms; every other gate is identical, and the emit
    /// is byte-for-byte the same.
    fn clone_escaping_live_local_enum_core(
        &mut self,
        scrutinee: &Expr,
        val: BasicValueEnum<'ctx>,
        patterns: &[&Pattern],
        escapes: bool,
        live: &dyn Fn(&Self, &str) -> bool,
    ) -> (BasicValueEnum<'ctx>, bool) {
        // Identifier only, for the same owned-`self` reason
        // `scrutinee_is_readonly_owned_enum_local` documents.
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return (val, false);
        };
        // The loop-element leg owns its own population.
        if self
            .borrow_vars
            .for_loop_owned_agg_vars
            .contains(name.as_str())
        {
            return (val, false);
        }
        // ESCAPING bindings only — the read-only case is the borrow path above.
        if !escapes {
            return (val, false);
        }
        let Some(slot) = self.variables.get(name.as_str()).copied() else {
            return (val, false);
        };
        let Some(enum_name) = self.var_types.var_type_names.get(name.as_str()).cloned() else {
            return (val, false);
        };
        let Some(layout) = self.type_decls.enum_layouts.get(&enum_name).cloned() else {
            return (val, false);
        };
        if layout.is_shared {
            return (val, false);
        }
        // Same ownership witness the read-only classifier uses: the source must
        // provably still free its own copy, or the clone is a leak.
        if !self.slot_has_live_enum_drop(slot.ptr) {
            return (val, false);
        }
        if !patterns
            .iter()
            .any(|p| self.enum_pattern_binds_heap_payload(&enum_name, p))
        {
            return (val, false);
        }
        if !self.enum_payload_clone_is_faithful(&enum_name, &layout) {
            return (val, false);
        }
        if !live(self, name) {
            return (val, false);
        }
        let fn_val = self.current_fn.unwrap();
        let ll = val.get_type();
        let clone = self.create_entry_alloca(fn_val, "livelocal.enum.clone", ll);
        self.builder.build_store(clone, val).unwrap();
        // ELEMENT-deep (B-2026-08-09-9): an outer-only copy leaves a
        // `Vec[String]` payload's element buffers shared with the source, and
        // the consuming arm then frees buffers the live source still owns.
        // Sound here specifically because this clone exists only when the arm's
        // payload ESCAPES into a consumer, so the copied elements have an owner.
        self.deep_copy_enum_heap_payload_in_place(&enum_name, clone, &layout);
        (
            self.builder
                .build_load(ll, clone, "livelocal.enum.cloned")
                .unwrap(),
            true,
        )
    }

    /// B-2026-08-09-9 — is duplicating `enum_name`'s payload FAITHFUL: two
    /// independently-freeable copies, and no observable side effect from the
    /// copy itself?
    ///
    /// Two separate hazards, and the second is easy to miss because it is not a
    /// memory bug at all:
    ///
    /// - INDEPENDENCE. Handled for `VecOrString` by
    ///   `deep_copy_enum_heap_payload_in_place`, but only as deep
    ///   as `emit_vecstr_defensive_copy` walks — `String` and `Vec` inners. A
    ///   `Vec` of user STRUCTS would come back with the elements aliased, so
    ///   the inner is restricted to what is actually duplicated.
    ///
    /// - OBSERVABILITY. A `NestedStruct` payload may carry a user `impl Drop`,
    ///   and duplicating it makes that body run TWICE — a clone is a
    ///   compiler-internal artifact the source language never asked for.
    ///   Measured: `e2e_own_drop_enum_reassign_sequencing` gained a second
    ///   `drop 5 l5` / `loud drop` pair. So `NestedStruct` is excluded
    ///   regardless of whether its copy would be memory-safe, and `MapOrSet`
    ///   with it (unmeasured — status quo rather than a guess).
    ///
    /// This is the widened successor of the gate that shipped with legs 2/3:
    /// that one also excluded `Vec` of heap elements, because the duplicator
    /// was outer-only then. Now that it can go element-deep, that class is
    /// admitted and the exclusions are the two above.
    fn enum_payload_clone_is_faithful(
        &self,
        enum_name: &str,
        layout: &crate::codegen::EnumLayout<'ctx>,
    ) -> bool {
        let variant_tes: std::collections::HashMap<String, Vec<TypeExpr>> = self
            .enum_variant_field_type_exprs(enum_name)
            .into_iter()
            .map(|(_tag, name, tes)| (name, tes))
            .collect();
        layout.field_drop_kinds.iter().all(|(variant, kinds)| {
            let kinds: &Vec<crate::codegen::state::EnumDropKind> = kinds;
            kinds.iter().enumerate().all(|(fi, kind)| {
                if !kind.is_heap_bearing() {
                    return true;
                }
                if *kind != crate::codegen::state::EnumDropKind::VecOrString {
                    return false;
                }
                let Some(te) = variant_tes.get(variant).and_then(|tes| tes.get(fi)) else {
                    return false;
                };
                if self.is_string_type_expr(te) {
                    return true;
                }
                // A `Vec`/`VecDeque` whose elements the defensive copy really
                // duplicates: a scalar (bit-copy is exact) or a `String`.
                super::helpers::vec_inner_type_expr(te).is_some_and(|elem| {
                    super::vec_method::is_trivially_copyable_te(&elem)
                        || self.is_string_type_expr(&elem)
                })
            })
        })
    }

    /// B-2026-07-21-5/-6: `match <refparam>.field { Ident(name) => <consume
    /// name> … }` — a field chain rooted at a `ref`/`mut ref` param (incl. a
    /// `ref self` receiver) whose leaf is a heap-bearing non-shared user enum,
    /// where an arm MOVES a payload binding out. The field read through the
    /// borrow is a bit-copy alias of the CALLER's storage, and the owned-path
    /// source suppression can't fire against a borrowed root
    /// (`field_chain_place_ptr` bails there — zeroing the caller's value is
    /// not the callee's to do, and GEPing the pointer slot was the original
    /// out-of-bounds corruption). Deep-clone the scrutinee value instead so
    /// each binding owns an INDEPENDENT buffer — the borrowed-chain sibling of
    /// `clone_escaping_owned_agg_loop_var_enum`, with the same contract:
    /// `did_clone = true` forces the clone through
    /// `materialize_freshtemp_enum_scrutinee`, so a no-bind arm frees the
    /// clone and a consuming arm's per-field suppression zeroes the CLONE's
    /// moved-out caps. The caller's original is untouched (it frees its own
    /// payload exactly once), matching interpreter semantics. Gated to the
    /// ESCAPING case — a read-only match is classified as a borrow
    /// (`scrutinee_is_readonly_borrowed_place`) and stays a zero-cost alias.
    pub(super) fn clone_escaping_borrowed_ref_chain_enum(
        &mut self,
        scrutinee: &Expr,
        val: BasicValueEnum<'ctx>,
        escapes: bool,
    ) -> (BasicValueEnum<'ctx>, bool) {
        if !matches!(
            scrutinee.kind,
            ExprKind::FieldAccess { .. } | ExprKind::TupleIndex { .. }
        ) {
            return (val, false);
        }
        // Strict place walk: FieldAccess/TupleIndex hops only, rooted DIRECTLY
        // at the ref param. An `Index` hop anywhere in the chain means the
        // source lives in a heap buffer the existing machinery already
        // handles — `toks[j].tok` via the #18 element suppression
        // (`vec_index_elem_ptr` resolves through the data pointer, not the
        // pointer slot), `self.toks[i].tok` via the #38 index-field clone —
        // and cloning here TOO would orphan the other copy into a leak
        // (caught by the borrowed-index LSan trio when this walk used the
        // Index-transparent `place_root_ident`).
        let mut cur = scrutinee;
        let root = loop {
            match &cur.kind {
                ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                    cur = object;
                }
                ExprKind::Identifier(n) => break n.as_str(),
                ExprKind::SelfValue => break "self",
                _ => return (val, false),
            }
        };
        if !self.borrow_vars.signature_ref_params.contains(root) {
            return (val, false);
        }
        if !escapes {
            return (val, false);
        }
        let Some(enum_name) = self.place_chain_type_name(scrutinee) else {
            return (val, false);
        };
        let Some(layout) = self.type_decls.enum_layouts.get(&enum_name).cloned() else {
            return (val, false);
        };
        // Shared enums are RC-boxed (the rc-inc/dec machinery owns them);
        // seeded Option/Result have all-`None` drop kinds and self-gate via
        // `materialize_freshtemp_enum_scrutinee` returning None, so cloning
        // them here would leak the clone — user value enums only.
        if layout.is_shared
            || !layout
                .field_drop_kinds
                .values()
                .flatten()
                .any(|k| k.is_heap_bearing())
        {
            return (val, false);
        }
        let fn_val = self.current_fn.unwrap();
        let ll = val.get_type();
        let slot = self.create_entry_alloca(fn_val, "refchain.enum.clone", ll);
        self.builder.build_store(slot, val).unwrap();
        // B-2026-08-09-12 — ELEMENT-deep, for the same reason the live-local
        // sibling is: the outer-only copy left a `Vec[String]` payload's element
        // buffers shared with the CALLER's storage, so a consuming arm freed
        // buffers the caller still owned. Measured on `fn take(s: ref S)` over
        // `S { e: E }`, `enum E { A(Vec[String]) }`, called twice — one invalid
        // read and one invalid free.
        //
        // Sound at this site on the same terms: the leg fires only when the
        // arm's bindings ESCAPE (`ref_chain_escapes`), so the copied elements
        // go to a consumer. Payload classes the defensive copy does not
        // duplicate (a `Vec` of user structs, a `NestedStruct`) are unaffected
        // — they alias exactly as before, which is this leg's status quo rather
        // than something this change extends.
        self.deep_copy_enum_heap_payload_in_place(&enum_name, slot, &layout);
        (
            self.builder
                .build_load(ll, slot, "refchain.enum.cloned")
                .unwrap(),
            true,
        )
    }

    /// B-2026-07-21-7 — the STRUCT-leaf sibling of
    /// [`Self::clone_escaping_borrowed_ref_chain_enum`]: `match
    /// <refparam>.field { Pt { s, x } => <consume s> … }` where the leaf field
    /// is a heap-bearing non-shared user STRUCT. The field read through the
    /// borrow bit-copy-aliases the caller's struct; with an ESCAPING arm the
    /// bindings get owned tracking, and the source suppression cannot fire
    /// against a borrowed root (`field_chain_place_ptr` bails) — so binding
    /// and caller both freed the same buffer (double-free; the "read-only"
    /// `s.len() + x` shape hits it too, because the `iN.add` desugar makes the
    /// scalar `x` a call arg the escape walk counts as a move). Deep-clone the
    /// scrutinee value and register the clone's own `StructDrop`; each arm's
    /// per-field suppression then fires against the CLONE
    /// (`suppress_destructured_struct_pattern_cleanup_at`), so consumed fields
    /// are owned by their bindings, unbound fields are freed by the clone
    /// drop, and the caller's struct is untouched — interpreter semantics.
    ///
    /// Gates: pure FieldAccess/TupleIndex chain rooted DIRECTLY at a
    /// `ref`/`mut ref` param (no `Index` hop — element sources are other
    /// machinery's, mirroring the enum sibling); some arm escapes; every arm
    /// is a `Struct` pattern of the leaf type or a wildcard (a whole-value
    /// binding arm would alias the entire clone — status quo there); the
    /// leaf's clone/drop family fully supports the shape
    /// (`borrow_payload_clone_supported` — a partial clone is never emitted).
    /// Returns the (possibly cloned) scrutinee value plus the clone slot +
    /// struct name for the per-arm suppression.
    pub(super) fn clone_escaping_borrowed_ref_chain_struct(
        &mut self,
        scrutinee: &Expr,
        val: BasicValueEnum<'ctx>,
        patterns: &[&Pattern],
        escapes: bool,
    ) -> (BasicValueEnum<'ctx>, Option<(PointerValue<'ctx>, String)>) {
        if !matches!(
            scrutinee.kind,
            ExprKind::FieldAccess { .. } | ExprKind::TupleIndex { .. }
        ) {
            return (val, None);
        }
        let mut cur = scrutinee;
        let root = loop {
            match &cur.kind {
                ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                    cur = object;
                }
                ExprKind::Identifier(n) => break n.as_str(),
                ExprKind::SelfValue => break "self",
                _ => return (val, None),
            }
        };
        if !self.borrow_vars.signature_ref_params.contains(root) {
            return (val, None);
        }
        if !escapes {
            return (val, None);
        }
        let Some(struct_name) = self.place_chain_type_name(scrutinee) else {
            return (val, None);
        };
        if !self
            .type_decls
            .struct_types
            .contains_key(struct_name.as_str())
            || self
                .type_decls
                .shared_types
                .contains_key(struct_name.as_str())
            || self
                .type_decls
                .enum_layouts
                .contains_key(struct_name.as_str())
        {
            return (val, None);
        }
        let all_patterns_struct_or_wild = patterns.iter().all(|p| match &p.kind {
            PatternKind::Struct { path, .. } => {
                path.last().map(|s| s.as_str()) == Some(struct_name.as_str())
            }
            PatternKind::Wildcard => true,
            _ => false,
        });
        if !all_patterns_struct_or_wild {
            return (val, None);
        }
        let te = TypeExpr {
            kind: TypeKind::Path(crate::ast::PathExpr {
                segments: vec![struct_name.clone()],
                generic_args: None,
                span: scrutinee.span,
            }),
            span: scrutinee.span,
        };
        if !self.borrow_payload_clone_supported(&te) {
            return (val, None);
        }
        let fn_val = self.current_fn.unwrap();
        let ll = val.get_type();
        let clone_fn = self.emit_clone_fn_for_type_expr(&te);
        let src = self.create_entry_alloca(fn_val, "refchain.struct.src", ll);
        self.builder.build_store(src, val).unwrap();
        let slot = self.create_entry_alloca(fn_val, "refchain.struct.clone", ll);
        self.builder
            .build_call(clone_fn, &[src.into(), slot.into()], "")
            .unwrap();
        self.track_struct_var(&struct_name, slot);
        (
            self.builder
                .build_load(ll, slot, "refchain.struct.cloned")
                .unwrap(),
            Some((slot, struct_name)),
        )
    }

    /// B-2026-07-21-9 — the OPTION-leaf sibling of
    /// [`Self::clone_escaping_borrowed_ref_chain_enum`]: `match
    /// <refparam>.field { Some(s) => <consume s> … }` where the leaf field is
    /// an `Option[String]`/`Option[Vec[U]]` (inline heap payload). The enum
    /// clone leg gates seeded Option out (all-`None` drop kinds, no freshtemp
    /// channel), so the inline payload binding aliased the caller's buffer
    /// and both freed it. Deep-clone the Option value (the dispatcher's
    /// tag-guarded `emit_option_value_clone_fn`), register a
    /// `FreeInlineOptionPayload` on the clone slot, and let the CONSUMING
    /// `Some` arm zero the clone's TAG (`zero_option_field_tag_at`) so the
    /// cleanup skips the payload the binding now owns; a non-consuming arm
    /// (`None` / `_`) leaves the tag and the cleanup frees the clone's
    /// payload. Caller's value untouched — interpreter semantics.
    ///
    /// Gates mirror the siblings: pure FieldAccess chain rooted at a
    /// `ref`/`mut ref` param (the leaf hop must be a named field — the
    /// payload te comes from the owning struct's field table), an escaping
    /// binding, and an INLINE heap payload (`option_inline_payload_elem`;
    /// boxed/shared/Map payloads keep the status quo — they are other
    /// cleanup families). Returns the (possibly cloned) value plus the clone
    /// slot for the consuming-arm tag zero.
    pub(super) fn clone_escaping_borrowed_ref_chain_option(
        &mut self,
        scrutinee: &Expr,
        val: BasicValueEnum<'ctx>,
        escapes: bool,
    ) -> (BasicValueEnum<'ctx>, Option<PointerValue<'ctx>>) {
        let ExprKind::FieldAccess { object, field } = &scrutinee.kind else {
            return (val, None);
        };
        let mut cur = object.as_ref();
        let root = loop {
            match &cur.kind {
                ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                    cur = object;
                }
                ExprKind::Identifier(n) => break n.as_str(),
                ExprKind::SelfValue => break "self",
                _ => return (val, None),
            }
        };
        if !self.borrow_vars.signature_ref_params.contains(root) {
            return (val, None);
        }
        if !escapes {
            return (val, None);
        }
        let Some(obj_ty) = self.place_chain_type_name(object) else {
            return (val, None);
        };
        let Some(idx) = self
            .type_decls
            .struct_field_names
            .get(obj_ty.as_str())
            .and_then(|ns| ns.iter().position(|n| n == field))
        else {
            return (val, None);
        };
        let Some(field_te) = self
            .type_decls
            .struct_field_type_exprs
            .get(obj_ty.as_str())
            .and_then(|tes| tes.get(idx))
            .cloned()
        else {
            return (val, None);
        };
        // Inline String/Vec payloads only — a shared payload is rc-managed,
        // a boxed/Map payload belongs to other cleanup families.
        let Some(payload_elem_ty) = self.option_inline_payload_elem(&field_te) else {
            return (val, None);
        };
        if self
            .option_inner_shared_type_for_type_expr(&field_te)
            .is_some()
        {
            return (val, None);
        }
        let Some(layout) = self.type_decls.enum_layouts.get("Option") else {
            return (val, None);
        };
        let option_ty = layout.llvm_type;
        let some_tag = layout.tags.get("Some").copied().unwrap_or(1);
        let fn_val = self.current_fn.unwrap();
        let ll = val.get_type();
        let clone_fn = self.emit_clone_fn_for_type_expr(&field_te);
        let src = self.create_entry_alloca(fn_val, "refchain.opt.src", ll);
        self.builder.build_store(src, val).unwrap();
        let slot = self.create_entry_alloca(fn_val, "refchain.opt.clone", ll);
        self.builder
            .build_call(clone_fn, &[src.into(), slot.into()], "")
            .unwrap();
        // B-2026-08-14-15 leg B — an aggregate-element `Vec[P]` payload needs
        // its per-element drain too; the clone above duplicated each element's
        // own heap, so the overlay's outer-buffer free alone strands it.
        let payload_elem_agg_drop = self.option_payload_vec_elem_agg_drop(&field_te);
        if let Some(frame) = self.drop_rc.scope_cleanup_actions.last_mut() {
            frame.push(
                crate::codegen::state::CleanupAction::FreeInlineOptionPayload {
                    option_slot: slot,
                    option_ty,
                    some_tag,
                    payload_elem_ty: Some(payload_elem_ty),
                    payload_elem_agg_drop,
                },
            );
        }
        (
            self.builder
                .build_load(ll, slot, "refchain.opt.cloned")
                .unwrap(),
            Some(slot),
        )
    }

    /// B-2026-08-08-25 leg 1 — the LIVE-LOCAL sibling of
    /// [`Self::clone_escaping_borrowed_ref_chain_option`]: `match o { Some(s)
    /// => <CONSUME s> … }` where `o` is a plain local that owns an inline
    /// `Option[String]` / `Option[Vec[U]]` payload and is READ AGAIN after the
    /// match.
    ///
    /// The consuming arm moves the payload out of the source, and
    /// `suppress_inline_option_payload_cleanup` disarms the source so the
    /// buffer is freed once — by the arm. That is correct while the source is
    /// dead, and it is the whole defect while the source is live: the arm's
    /// free lands before the later read, and the still-in-scope `o` is left
    /// pointing at freed memory (`Invalid read of size N`, 7 valgrind errors
    /// on the row's reproduction).
    ///
    /// The BORROWING half of this shape is already free —
    /// `scrutinee_is_readonly_inline_optres_local` proves the arms only read
    /// the payload and leaves it with the source at zero cost. That classifier
    /// correctly declines a consuming arm, because a genuine move needs a
    /// second buffer rather than a second reader. This is that second buffer.
    ///
    /// WHY A CLONE RATHER THAN A LIFETIME FIX. The row's last pass concluded
    /// from an IR diff that the alias is already balanced and only the free's
    /// TIMING is wrong, so suppressing the RESULT's free while the source is
    /// live would be cheaper. That works for a result that dies in the same
    /// scope and silently dangles for one that does not — a returned value, a
    /// pushed element, a field store — so it trades this use-after-free for a
    /// rarer one. Two live owners need two buffers; the clone is
    /// unconditionally sound, which is the direction to be wrong in on a path
    /// where the alternative failure is a double-free.
    ///
    /// The row also warned that a bespoke inline-payload clone must get
    /// element DEPTH right or a `Vec[String]` payload becomes a double-free.
    /// Nothing bespoke is needed: `emit_clone_fn_for_type_expr` is the
    /// dispatcher's own tag-guarded, type-directed clone (the same one the
    /// ref-chain sibling above uses), so depth follows the `TypeExpr`. The
    /// `deep_copy_enum_heap_payload_in_place` trap the row recorded applies to
    /// the user-ENUM clone legs, which key on `field_drop_kinds` the erased
    /// Option layout does not carry — this leg never touches that path.
    ///
    /// Ownership after the clone, and why no leak: the clone slot registers
    /// its own `FreeInlineOptionPayload`, and a CONSUMING `Some` arm zeroes
    /// the clone's tag (`zero_refchain_option_clone_on_consume`, shared with
    /// the ref-chain leg) so that cleanup skips the buffer the binding now
    /// owns. A `None` or wildcard arm leaves the tag alone and the cleanup
    /// frees the clone. The source is untouched and keeps its own payload —
    /// the caller sets `pattern_binding_source_retains_inline_payload` so the
    /// suppressors do not disarm it. Two allocations, two frees, which is what
    /// `--interp` does.
    ///
    /// Gated to a source that is actually READ LATER
    /// (`scrutinee_read_after_match`). Every consuming match over a live local
    /// would otherwise pay a `malloc` + `memcpy` it does not need — the source
    /// is dead in the common case, and today's transfer is both correct and
    /// free there. That keeps this off the ~1000 memory fixtures that exercise
    /// the arm path and confines it to programs that are broken today.
    pub(super) fn clone_escaping_live_local_option(
        &mut self,
        scrutinee: &Expr,
        val: BasicValueEnum<'ctx>,
        arms: &[MatchArm],
    ) -> (BasicValueEnum<'ctx>, Option<PointerValue<'ctx>>) {
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return (val, None);
        };
        // A passthrough binding owns nothing — its source does. Resolve to the
        // owner exactly as the suppressors and classifiers forward
        // (B-2026-08-06-27), so the clone lands on the value that is really at
        // risk.
        let owner: String = self
            .payload_vars
            .passthrough_owner_alias
            .get(name.as_str())
            .cloned()
            .unwrap_or_else(|| name.clone());
        if !self
            .payload_vars
            .inline_option_payload_vars
            .contains(owner.as_str())
        {
            return (val, None);
        }
        // ESCAPING arms only. A read-only match is already the zero-cost borrow
        // path above, and cloning there would leak the clone (nothing consumes
        // it, and the binding registers no owner).
        if self.no_arm_payload_escapes(arms) {
            return (val, None);
        }
        // Same direct-`{ptr,len,cap}` shape gate the read-only classifier uses:
        // `inline_option_payload_vars` also carries TUPLE payloads, whose
        // bindings own themselves and must keep their own channel.
        if !arms
            .iter()
            .any(|a| self.pattern_binds_direct_inline_heap_payload(&a.pattern))
        {
            return (val, None);
        }
        if !arms.iter().all(|a| {
            !Self::pattern_consumes_variant_payload(&a.pattern)
                || self.pattern_binds_direct_inline_heap_payload(&a.pattern)
        }) {
            return (val, None);
        }
        if !self.scrutinee_read_after_match(name, &owner, scrutinee, arms) {
            return (val, None);
        }
        // The `Option[P]` instantiation, needed for the type-directed clone.
        // `enum_inst_var_types` is the name-keyed use-site table (span keys
        // collide across f-string interpolations); `optres_var_payload_tes`
        // covers a rebound binding that re-registered its payload walk.
        let Some(opt_te) = self
            .type_decls
            .enum_inst_var_types
            .get(owner.as_str())
            .or_else(|| self.var_types.optres_var_payload_tes.get(owner.as_str()))
            .cloned()
        else {
            return (val, None);
        };
        // Inline String/Vec payloads only — a shared payload is rc-managed and
        // a boxed one belongs to another cleanup family, exactly as the
        // ref-chain sibling gates.
        let Some(payload_elem_ty) = self.option_inline_payload_elem(&opt_te) else {
            return (val, None);
        };
        if self
            .option_inner_shared_type_for_type_expr(&opt_te)
            .is_some()
        {
            return (val, None);
        }
        let Some(layout) = self.type_decls.enum_layouts.get("Option") else {
            return (val, None);
        };
        let option_ty = layout.llvm_type;
        let some_tag = layout.tags.get("Some").copied().unwrap_or(1);
        let fn_val = self.current_fn.unwrap();
        let ll = val.get_type();
        let clone_fn = self.emit_clone_fn_for_type_expr(&opt_te);
        let src = self.create_entry_alloca(fn_val, "livelocal.opt.src", ll);
        self.builder.build_store(src, val).unwrap();
        let slot = self.create_entry_alloca(fn_val, "livelocal.opt.clone", ll);
        self.builder
            .build_call(clone_fn, &[src.into(), slot.into()], "")
            .unwrap();
        // B-2026-08-14-15 leg B — an aggregate-element `Vec[P]` payload needs
        // its per-element drain too; the clone above duplicated each element's
        // own heap, so the overlay's outer-buffer free alone strands it.
        let payload_elem_agg_drop = self.option_payload_vec_elem_agg_drop(&opt_te);
        if let Some(frame) = self.drop_rc.scope_cleanup_actions.last_mut() {
            frame.push(
                crate::codegen::state::CleanupAction::FreeInlineOptionPayload {
                    option_slot: slot,
                    option_ty,
                    some_tag,
                    payload_elem_ty: Some(payload_elem_ty),
                    payload_elem_agg_drop,
                },
            );
        }
        (
            self.builder
                .build_load(ll, slot, "livelocal.opt.cloned")
                .unwrap(),
            Some(slot),
        )
    }

    /// B-2026-08-08-25 leg 2 — the `Result` twin of
    /// [`Self::clone_escaping_live_local_option`], for a consuming arm over a
    /// LIVE `Result` local.
    ///
    /// `let r: Result[String, i64] = Ok(f"hi"); match r.map(|s| s) { … }`
    /// followed by a re-read of `r` printed `hi` and then an EMPTY line: the
    /// heap-payload `.map` lowers via `compile_map_via_match_synthesis`, whose
    /// synthesized `match` has the RECEIVER as its scrutinee, so this is the
    /// same bare-local consuming match leg 1 fixed — it was simply gated to
    /// `inline_option_payload_vars` and never looked at the `Result` set. Empty
    /// rather than garbage (and valgrind-clean) because the transfer path zeros
    /// the source's payload words rather than freeing behind it: one buffer,
    /// one free, and a source that reads back as `{0, 0, 0}`.
    ///
    /// Registration order and the clone-slot contract are the ref-chain
    /// sibling's, not leg 1's: `track_inline_result_payload_var` BEFORE the
    /// value store and the deep copy (its nested-block zero-init targets the
    /// entry block, and once the copy has split blocks that test would wrongly
    /// fire and wipe the clone at runtime), and the per-arm
    /// `suppress_inline_result_payload_cleanup_at` zeroes the consumed half in
    /// the CLONE so the buffer the binding now owns is not freed twice.
    ///
    /// Gated on liveness by the same `scrutinee_read_after_match`, so a source
    /// that is dead after the match keeps today's zero-cost transfer.
    pub(super) fn clone_escaping_live_local_result(
        &mut self,
        scrutinee: &Expr,
        val: BasicValueEnum<'ctx>,
        arms: &[MatchArm],
    ) -> (BasicValueEnum<'ctx>, Option<PointerValue<'ctx>>) {
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return (val, None);
        };
        let owner: String = self
            .payload_vars
            .passthrough_owner_alias
            .get(name.as_str())
            .cloned()
            .unwrap_or_else(|| name.clone());
        if !self
            .payload_vars
            .inline_result_payload_vars
            .contains(owner.as_str())
        {
            return (val, None);
        }
        // ESCAPING arms only — a read-only match is the borrow path above, and
        // cloning there would leak the clone.
        if self.no_arm_payload_escapes(arms) {
            return (val, None);
        }
        if !arms
            .iter()
            .any(|a| self.pattern_binds_direct_inline_heap_payload(&a.pattern))
        {
            return (val, None);
        }
        if !self.scrutinee_read_after_match(name, &owner, scrutinee, arms) {
            return (val, None);
        }
        let Some(res_te) = self
            .type_decls
            .enum_inst_var_types
            .get(owner.as_str())
            .or_else(|| self.var_types.optres_var_payload_tes.get(owner.as_str()))
            .cloned()
        else {
            return (val, None);
        };
        // Per-half shape gate shared with the ref-chain leg: both halves direct
        // inline-heap `String`/`Vec` or heap-free scalar, at least one heap.
        // Anything else belongs to another cleanup family.
        if !self.result_field_direct_vecstr_halves_ok(&res_te) {
            return (val, None);
        }
        if self.result_inline_payload_elems(&res_te).is_none() {
            return (val, None);
        }
        let fn_val = self.current_fn.unwrap();
        let ll = val.get_type();
        let slot = self.create_entry_alloca(fn_val, "livelocal.res.clone", ll);
        self.track_inline_result_payload_var("__livelocal_res_tmp", slot, &res_te);
        self.builder.build_store(slot, val).unwrap();
        self.deep_copy_result_inline_heap_halves_in_place(slot, &res_te);
        (
            self.builder
                .build_load(ll, slot, "livelocal.res.cloned")
                .unwrap(),
            Some(slot),
        )
    }

    /// Is `name` (or the `owner` it aliases) mentioned at or past the END of
    /// this match — i.e. does the scrutinee local outlive the arm that moves
    /// its payload out? B-2026-08-08-25 leg 1's liveness gate.
    ///
    /// Mentions BEFORE the match are deliberately not counted. A read that
    /// already happened cannot observe a buffer the arm frees afterwards, so
    /// counting it would clone for `println(o.is_some()); match o { … }` — a
    /// dead source, correct and free on today's transfer path.
    ///
    /// The scrutinee's own mention sits inside `[scrutinee.start, match_end)`
    /// and so never triggers the gate by itself, which is the entire point:
    /// `match o { … }` alone keeps the transfer, a second `match o { … }` gets
    /// the clone.
    fn scrutinee_read_after_match(
        &self,
        name: &str,
        owner: &str,
        scrutinee: &Expr,
        arms: &[MatchArm],
    ) -> bool {
        // The arm bodies end after the patterns and any guard, so the last
        // body's end bounds the whole match. The scrutinee's own extent is
        // folded in for a degenerate/synthesized arm span (the `map` synthesis
        // reuses the call's spans), keeping the bound from landing before the
        // scrutinee and reading its own mention as a later use.
        let match_end = arms
            .iter()
            .map(|a| a.body.span.offset + a.body.span.length)
            .chain(std::iter::once(
                scrutinee.span.offset + scrutinee.span.length,
            ))
            .max()
            .unwrap_or(0);
        self.ident_read_at_or_after(name, owner, match_end)
    }

    /// B-2026-08-09-11 — the BLOCK-form sibling of
    /// [`Self::scrutinee_read_after_match`], for an `if let` / `while let`
    /// whose construct ends at a block rather than at a list of arm bodies.
    ///
    /// `else_end` folds in an `else` branch's extent so `if let … { } else {
    /// … <read e> … }` is not read as a use past the construct: a mention
    /// inside the else arm is part of the construct, not after it. Passing
    /// `None` (no else, or `while let`) bounds on the block alone.
    ///
    /// The `while let` caller bounds on the loop BODY, which is the same
    /// conservative direction as the match form: a read inside the body is
    /// within `[scrutinee.start, end)` and so does not by itself arm the
    /// clone, while a read after the loop does.
    fn scrutinee_read_after_block(
        &self,
        name: &str,
        owner: &str,
        scrutinee: &Expr,
        block: &crate::ast::Block,
        else_end: Option<usize>,
    ) -> bool {
        let end = [
            block.span.offset + block.span.length,
            scrutinee.span.offset + scrutinee.span.length,
        ]
        .into_iter()
        .chain(else_end)
        .max()
        .unwrap_or(0);
        self.ident_read_at_or_after(name, owner, end)
    }

    /// Shared core of the two liveness gates: is `name` (or the `owner` it
    /// aliases) mentioned at or past `end`?
    ///
    /// Either spelling can carry the later read: the source may be reached
    /// through the passthrough alias as well as its own name.
    fn ident_read_at_or_after(&self, name: &str, owner: &str, end: usize) -> bool {
        [name, owner]
            .iter()
            .filter_map(|n| self.fn_sig.fn_body_ident_mention_offsets.get(*n))
            .flatten()
            .any(|&off| off >= end)
    }

    /// Index every expression-level identifier mention in the function body
    /// about to be compiled, by source offset. Feeds
    /// [`Self::scrutinee_read_after_match`] (B-2026-08-08-25 leg 1); see the
    /// field's doc on `fn_body_ident_mention_offsets` for why offsets rather
    /// than counts, and why the walk's exhaustiveness matters.
    pub(super) fn record_fn_body_ident_mentions(&mut self, body: &crate::ast::Block) {
        type Acc = std::collections::HashMap<String, Vec<usize>>;
        fn walk(e: &Expr, acc: &std::cell::RefCell<Acc>) -> bool {
            match &e.kind {
                ExprKind::Identifier(n) => {
                    acc.borrow_mut()
                        .entry(n.clone())
                        .or_default()
                        .push(e.span.offset);
                }
                ExprKind::Path { segments, .. } => {
                    if let Some(s) = segments.first() {
                        acc.borrow_mut()
                            .entry(s.clone())
                            .or_default()
                            .push(e.span.offset);
                    }
                }
                _ => {}
            }
            // Always true — the walk visits rather than tests, so every child
            // is reached instead of short-circuiting on the first hit.
            crate::codegen::bce_length_pin::expr_children_all(e, |c| walk(c, acc))
        }
        let acc: std::cell::RefCell<Acc> = std::cell::RefCell::new(Acc::new());
        crate::codegen::bce_length_pin::block_all(body, |e| walk(e, &acc));
        self.fn_sig.fn_body_ident_mention_offsets = acc.into_inner();
    }

    /// B-2026-07-21-14 — the RESULT-leaf sibling of
    /// [`Self::clone_escaping_borrowed_ref_chain_option`]: `match
    /// <refparam>.field { Ok(s) => <consume s> … }` where the leaf field is a
    /// `Result[T, E]` with at least one DIRECT inline-heap `String`/`Vec`
    /// half. The consuming arm's binding aliased the caller's buffer (the
    /// source suppressions bail on the borrowed root), so the binding's
    /// scope-exit free and the caller's struct drop both freed it. Deep-clone
    /// the live half in place (`deep_copy_result_inline_heap_halves_in_place`
    /// — the `FreeInlineResultPayload` overlay words), register the clone
    /// slot's cleanup via `track_inline_result_payload_var`, and let each
    /// CONSUMING `Ok`/`Err` arm zero the clone's payload area
    /// (`suppress_inline_result_payload_cleanup_at`) so the cleanup skips the
    /// buffer the binding now owns; a no-bind arm leaves it armed and the
    /// clone's payload frees at scope exit. Caller's value untouched —
    /// interpreter copy semantics.
    ///
    /// Gates mirror the Option sibling: pure FieldAccess chain rooted at a
    /// `ref`/`mut ref` param, an escaping binding, and each half either a
    /// DIRECT inline-heap `String`/`Vec` (with a non-shared element) or a
    /// heap-free scalar — shared halves, struct/wrapper halves, and nested
    /// seeded-enum halves keep the status quo (other cleanup families).
    pub(super) fn clone_escaping_borrowed_ref_chain_result(
        &mut self,
        scrutinee: &Expr,
        val: BasicValueEnum<'ctx>,
        escapes: bool,
    ) -> (BasicValueEnum<'ctx>, Option<PointerValue<'ctx>>) {
        let ExprKind::FieldAccess { object, field } = &scrutinee.kind else {
            return (val, None);
        };
        let mut cur = object.as_ref();
        let root = loop {
            match &cur.kind {
                ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                    cur = object;
                }
                ExprKind::Identifier(n) => break n.as_str(),
                ExprKind::SelfValue => break "self",
                _ => return (val, None),
            }
        };
        if !self.borrow_vars.signature_ref_params.contains(root) {
            return (val, None);
        }
        if !escapes {
            return (val, None);
        }
        let Some(obj_ty) = self.place_chain_type_name(object) else {
            return (val, None);
        };
        let Some(idx) = self
            .type_decls
            .struct_field_names
            .get(obj_ty.as_str())
            .and_then(|ns| ns.iter().position(|n| n == field))
        else {
            return (val, None);
        };
        let Some(field_te) = self
            .type_decls
            .struct_field_type_exprs
            .get(obj_ty.as_str())
            .and_then(|tes| tes.get(idx))
            .cloned()
        else {
            return (val, None);
        };
        if !self.result_field_direct_vecstr_halves_ok(&field_te) {
            return (val, None);
        }
        // The cleanup registration below keys off the same extraction — make
        // sure it will actually arm (defensive; direct String/Vec always has
        // an overlay elem).
        if self.result_inline_payload_elems(&field_te).is_none() {
            return (val, None);
        }
        let fn_val = self.current_fn.unwrap();
        let ll = val.get_type();
        let slot = self.create_entry_alloca(fn_val, "refchain.res.clone", ll);
        // Register the cleanup BEFORE the deep copy emits IR: the tracker's
        // nested-block defensive zero-init targets the ENTRY block, and once
        // the copy has split blocks the current-block != entry test would
        // wrongly fire it — landing the zero between the value store below
        // and the copy's tag read, wiping the clone at runtime.
        self.track_inline_result_payload_var("__refchain_res_tmp", slot, &field_te);
        self.builder.build_store(slot, val).unwrap();
        self.deep_copy_result_inline_heap_halves_in_place(slot, &field_te);
        (
            self.builder
                .build_load(ll, slot, "refchain.res.cloned")
                .unwrap(),
            Some(slot),
        )
    }

    /// B-2026-07-21-11 — the LET-move sibling of the ref-chain clone family:
    /// `let p = <refparam>.field;` where the field is heap-bearing — a
    /// non-shared user STRUCT or ENUM, a `String`/`Vec`/`VecDeque`, or an
    /// `Option` with an inline heap payload. The field read through the
    /// borrow bit-copy-aliases the caller's field; the binding then gets
    /// owned tracking below the Let's RHS compile, and the #16/#19 source
    /// suppressions bail on the borrowed root (the ref param's slot holds a
    /// POINTER, not the struct) — so the binding and the caller's struct drop
    /// freed the same heap. Deep-copy the compiled value IN PLACE so the
    /// binding owns an independent copy and every downstream registration
    /// behaves as if the RHS were a fresh owned temp — interpreter copy
    /// semantics; the caller's field is untouched. Same shape gates as the
    /// scrutinee clones: a pure FieldAccess/TupleIndex chain (no `Index` hop)
    /// rooted DIRECTLY at a `ref`/`mut ref` param, leaf clone-family
    /// supported (`Result` fields stay status quo — the dispatcher has no
    /// Result deep clone). Unconditional on use (a let-bound copy outlives
    /// analysis reach cheaply enough — the shape itself is the move).
    pub(super) fn clone_ref_chain_field_move_rhs(
        &mut self,
        value: &Expr,
        val: BasicValueEnum<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        if !matches!(
            value.kind,
            ExprKind::FieldAccess { .. } | ExprKind::TupleIndex { .. }
        ) {
            return val;
        }
        let mut cur = value;
        let root = loop {
            match &cur.kind {
                ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                    cur = object;
                }
                ExprKind::Identifier(n) => break n.as_str(),
                ExprKind::SelfValue => break "self",
                _ => return val,
            }
        };
        if !self.borrow_vars.signature_ref_params.contains(root) {
            return val;
        }
        let Some(leaf) = self.place_chain_type_name(value) else {
            return val;
        };
        if self.type_decls.shared_types.contains_key(leaf.as_str()) {
            return val;
        }
        // Enum leaf: in-place payload deep-copy (same duplication the
        // scrutinee clone uses — copy-depth == drop-depth). Seeded
        // Option/Result are NOT this leg (their erased layouts carry
        // all-None drop kinds) — Option falls through to the field-te leg
        // below, Result stays status quo (no deep clone in the dispatcher).
        if !matches!(leaf.as_str(), "Option" | "Result") {
            if let Some(layout) = self.type_decls.enum_layouts.get(leaf.as_str()).cloned() {
                if layout.is_shared
                    || !layout
                        .field_drop_kinds
                        .values()
                        .flatten()
                        .any(|k| k.is_heap_bearing())
                {
                    return val;
                }
                let fn_val = self.current_fn.unwrap();
                let ll = val.get_type();
                let slot = self.create_entry_alloca(fn_val, "refchain.letmove.enum", ll);
                self.builder.build_store(slot, val).unwrap();
                self.deep_copy_enum_heap_payload_in_place(&leaf, slot, &layout);
                return self
                    .builder
                    .build_load(ll, slot, "refchain.letmove.enum.copy")
                    .unwrap();
            }
        }
        // String / Vec / VecDeque / Option leaf: the full field te (element /
        // payload types included) comes from the owning struct's field table
        // — the name-based leaf resolution loses `Vec[U]`'s element and
        // `Option[T]`'s payload. Same double-free shape: the bit-copy aliased
        // the caller's buffer while the Let registers an owned cleanup below.
        let te = if matches!(
            leaf.as_str(),
            "String" | "str" | "Vec" | "VecDeque" | "Option"
        ) {
            // B-2026-08-28-37 — the leaf may be a TUPLE MEMBER, not a struct
            // field: `let s = t.pair.0;` where `t: ref Wt` and the field is a
            // `(String, i64)`. This branch required a `FieldAccess` at the
            // outermost position, so a tuple-member leaf bailed here and the
            // binding aliased the borrowed struct's buffer — double-freeing
            // exactly as the field spelling did before B-2026-07-21-11. Both
            // leaf shapes want the same thing, the leaf's declared `TypeExpr`;
            // only the table it comes from differs (the owning struct's field
            // table vs the tuple's element `TypeExpr`s).
            let Some(fte) = (match &value.kind {
                ExprKind::FieldAccess { object, field } => {
                    self.place_chain_type_name(object).and_then(|obj_ty| {
                        self.type_decls
                            .struct_field_names
                            .get(obj_ty.as_str())
                            .and_then(|ns| ns.iter().position(|n| n == field))
                            .and_then(|idx| {
                                self.type_decls
                                    .struct_field_type_exprs
                                    .get(obj_ty.as_str())
                                    .and_then(|tes| tes.get(idx))
                            })
                            .cloned()
                    })
                }
                ExprKind::TupleIndex { object, index } => self
                    .place_chain_tuple_tes(object)
                    .and_then(|elems| elems.get(*index as usize).cloned()),
                _ => None,
            }) else {
                return val;
            };
            // B-2026-08-28-13: the field te is read from the GENERIC
            // declaration table, so inside a monomorph it still reads
            // `Vec[T]` — and `borrow_payload_clone_supported` then correctly
            // declines the unsubstituted param, skipping the clone SILENTLY
            // and leaving the binding aliasing the caller's buffer (the
            // binding's cleanup and the caller's struct-drop free it twice).
            // Substitute the active monomorph's type arguments first. A no-op
            // outside a monomorph (`type_subst_*` empty), so the non-generic
            // path — which already reached the clone — is bit-identical.
            let fte = self.subst_monomorph_type_params(&fte);
            // Option leaf: inline String/Vec payloads only — shared payloads
            // are rc-managed, boxed/Map payloads belong to other cleanup
            // families (mirrors the -9 scrutinee-clone gate).
            if leaf == "Option"
                && (self.option_inline_payload_elem(&fte).is_none()
                    || self.option_inner_shared_type_for_type_expr(&fte).is_some())
            {
                return val;
            }
            fte
        } else if self.type_decls.struct_types.contains_key(leaf.as_str()) {
            // Struct leaf: dispatcher deep clone keyed on the bare name.
            TypeExpr {
                kind: TypeKind::Path(crate::ast::PathExpr {
                    segments: vec![leaf.clone()],
                    generic_args: None,
                    span: value.span,
                }),
                span: value.span,
            }
        } else {
            return val;
        };
        if !self.borrow_payload_clone_supported(&te) {
            return val;
        }
        let fn_val = self.current_fn.unwrap();
        let ll = val.get_type();
        let clone_fn = self.emit_clone_fn_for_type_expr(&te);
        let src = self.create_entry_alloca(fn_val, "refchain.letmove.src", ll);
        self.builder.build_store(src, val).unwrap();
        let dst = self.create_entry_alloca(fn_val, "refchain.letmove.clone", ll);
        self.builder
            .build_call(clone_fn, &[src.into(), dst.into()], "")
            .unwrap();
        self.builder
            .build_load(ll, dst, "refchain.letmove.copy")
            .unwrap()
    }

    /// B-2026-07-21-10 — the TUPLE-leaf sibling: `match <refparam>.pair {
    /// (s, x) => <consume s> … }` where the leaf field is a tuple with heap
    /// elements. Same aliasing double-free as the other leaves; same recipe:
    /// deep-clone the tuple value (the dispatcher's per-element tuple clone),
    /// register the clone's own tuple `StructDrop`
    /// (`synthesize_tuple_drop_fn_te`), and let each consuming arm zero the
    /// consumed elements' caps in the CLONE slot
    /// (`zero_refchain_tuple_clone_on_consume` → `zero_tuple_elem_cap_at`)
    /// so bindings own their elements and the clone drop frees only unbound
    /// ones. Leaf te resolves from the owning struct's field table (the
    /// name-based leaf walk has no tuple name); gates mirror the siblings.
    #[allow(clippy::type_complexity)] // slot + agg ty + elem tes travel together to the per-arm zero
    pub(super) fn clone_escaping_borrowed_ref_chain_tuple(
        &mut self,
        scrutinee: &Expr,
        val: BasicValueEnum<'ctx>,
        escapes: bool,
    ) -> (
        BasicValueEnum<'ctx>,
        Option<(PointerValue<'ctx>, StructType<'ctx>, Vec<TypeExpr>)>,
    ) {
        let ExprKind::FieldAccess { object, field } = &scrutinee.kind else {
            return (val, None);
        };
        let mut cur = object.as_ref();
        let root = loop {
            match &cur.kind {
                ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                    cur = object;
                }
                ExprKind::Identifier(n) => break n.as_str(),
                ExprKind::SelfValue => break "self",
                _ => return (val, None),
            }
        };
        if !self.borrow_vars.signature_ref_params.contains(root) {
            return (val, None);
        }
        if !escapes {
            return (val, None);
        }
        let Some(obj_ty) = self.place_chain_type_name(object) else {
            return (val, None);
        };
        let Some(fte) = self
            .type_decls
            .struct_field_names
            .get(obj_ty.as_str())
            .and_then(|ns| ns.iter().position(|n| n == field))
            .and_then(|idx| {
                self.type_decls
                    .struct_field_type_exprs
                    .get(obj_ty.as_str())
                    .and_then(|tes| tes.get(idx))
            })
            .cloned()
        else {
            return (val, None);
        };
        let TypeKind::Tuple(elem_tes) = &fte.kind else {
            return (val, None);
        };
        let elem_tes = elem_tes.clone();
        if !elem_tes.iter().any(|e| self.te_owns_heap_below_buffer(e)) {
            return (val, None);
        }
        if !self.borrow_payload_clone_supported(&fte) {
            return (val, None);
        }
        let BasicTypeEnum::StructType(agg_ty) = val.get_type() else {
            return (val, None);
        };
        let Some(drop_fn) = self.synthesize_tuple_drop_fn_te(agg_ty, &elem_tes) else {
            return (val, None);
        };
        let fn_val = self.current_fn.unwrap();
        let ll = val.get_type();
        let clone_fn = self.emit_clone_fn_for_type_expr(&fte);
        let src = self.create_entry_alloca(fn_val, "refchain.tup.src", ll);
        self.builder.build_store(src, val).unwrap();
        let slot = self.create_entry_alloca(fn_val, "refchain.tup.clone", ll);
        self.builder
            .build_call(clone_fn, &[src.into(), slot.into()], "")
            .unwrap();
        if let Some(frame) = self.drop_rc.scope_cleanup_actions.last_mut() {
            frame.push(crate::codegen::state::CleanupAction::StructDrop {
                struct_alloca: slot,
                drop_fn,
            });
        }
        (
            self.builder
                .build_load(ll, slot, "refchain.tup.cloned")
                .unwrap(),
            Some((slot, agg_ty, elem_tes)),
        )
    }

    /// Consuming-arm companion of
    /// [`Self::clone_escaping_borrowed_ref_chain_tuple`]: zero each consumed
    /// element's caps in the clone slot so the clone's tuple drop frees only
    /// the elements this pattern left unbound.
    pub(super) fn zero_refchain_tuple_clone_on_consume(
        &mut self,
        slot: PointerValue<'ctx>,
        agg_ty: StructType<'ctx>,
        elem_tes: &[TypeExpr],
        pattern: &Pattern,
    ) {
        let PatternKind::Tuple(ps) = &pattern.kind else {
            return;
        };
        for (i, sub) in ps.iter().enumerate() {
            if !pattern_consumes_field(sub) {
                continue;
            }
            let Some(te) = elem_tes.get(i) else {
                continue;
            };
            if !self.te_owns_heap_below_buffer(te) {
                continue;
            }
            let te = te.clone();
            self.zero_tuple_elem_cap_at(slot, agg_ty, i as u32, &te);
        }
    }

    /// Consuming-`Some`-arm companion of
    /// [`Self::clone_escaping_borrowed_ref_chain_option`]: when the arm's
    /// pattern moves the `Some` payload into a binding, zero the clone slot's
    /// TAG so its `FreeInlineOptionPayload` cleanup skips the buffer the
    /// binding now owns. Non-consuming patterns (`None`, `_`, `Some(_)`)
    /// leave the tag — the cleanup frees the clone's payload.
    pub(super) fn zero_refchain_option_clone_on_consume(
        &mut self,
        clone_slot: PointerValue<'ctx>,
        pattern: &Pattern,
    ) {
        let PatternKind::TupleVariant { path, patterns } = &pattern.kind else {
            return;
        };
        if path.last().map(|s| s.as_str()) != Some("Some") {
            return;
        }
        if !patterns.first().is_some_and(pattern_consumes_field) {
            return;
        }
        self.zero_option_field_tag_at(clone_slot);
    }

    /// The B-2026-07-21-14 per-half `Result[T, E]` shape gate, shared by the
    /// ref-chain clone leg and the B-2026-07-21-16 owned-place move legs:
    /// every half must be a DIRECT inline-heap `String`/`Vec`/`VecDeque`
    /// (with a non-shared element) or a heap-free scalar, and at least one
    /// half must be heap. Anything else — shared, struct wrapper, user enum,
    /// nested Option/Map/tuple-with-heap — fails the gate so callers keep
    /// the status quo (those payload classes belong to other cleanup
    /// families, and the overlay copy/free pair only handles the
    /// `{ptr,len,cap}` class).
    pub(super) fn result_field_direct_vecstr_halves_ok(&self, field_te: &TypeExpr) -> bool {
        let TypeKind::Path(p) = &field_te.kind else {
            return false;
        };
        if p.segments.last().map(|s| s.as_str()) != Some("Result") {
            return false;
        }
        let Some(args) = p.generic_args.as_ref() else {
            return false;
        };
        let mut half_tes = Vec::with_capacity(2);
        for a in args.iter().take(2) {
            match a {
                crate::ast::GenericArg::Type(t) => half_tes.push(t),
                _ => return false,
            }
        }
        if half_tes.len() != 2 {
            return false;
        }
        let mut any_heap_half = false;
        for half in &half_tes {
            let is_direct_vecstr = self.is_string_type_expr(half)
                || (matches!(&half.kind, TypeKind::Path(hp)
                        if matches!(hp.segments.last().map(|s| s.as_str()), Some("Vec") | Some("VecDeque")))
                    && self.extract_vec_elem_type(half).is_some());
            if is_direct_vecstr {
                // A shared element would need per-element rc-incs the
                // defensive copy doesn't emit — keep that shape status quo.
                if crate::codegen::helpers::vec_inner_type_expr(half).is_some_and(|inner| {
                    matches!(&inner.kind, TypeKind::Path(ip)
                        if ip.segments.last().is_some_and(|n| self.type_decls.shared_types.contains_key(n.as_str())))
                }) {
                    return false;
                }
                any_heap_half = true;
            } else if self.te_owns_heap_below_buffer(half) {
                return false;
            }
        }
        any_heap_half
    }

    /// B-2026-08-03-3 leg B — the `Result` sibling of
    /// [`Self::option_payload_struct_or_enum_drop_ok`], and the DISJOINT
    /// twin of [`Self::result_field_direct_vecstr_halves_ok`]: at least one
    /// heap-owning half is a non-shared user struct/enum the recursive drop
    /// family fully frees, and every OTHER heap-owning half is one of those or
    /// a direct inline-heap `String`/`Vec`/`VecDeque`. Disjointness is
    /// structural and rests on the struct/enum REQUIREMENT: this gate demands
    /// at least one such half and the `..._direct_vecstr_halves_ok` gate
    /// rejects any, so each class keeps exactly one entry-copy helper
    /// (`deep_copy_result_struct_enum_payload_in_place` here,
    /// `deep_copy_result_inline_heap_halves_in_place` there) and copy-depth
    /// stays equal to the registered free's depth. Heapless halves (scalar /
    /// unit) are fine on either side: their arm emits neither copy nor free.
    ///
    /// The direct-half arm is B-2026-08-03-11: with both gates originally
    /// requiring their heap halves to be uniformly one kind, a MIXED
    /// `Result[<struct>, String]` field fell between them and its struct half
    /// leaked. Both free sides (`emit_result_drop_fn`, and
    /// `track_inline_result_payload_var` via `result_inline_payload_struct_drops`)
    /// already dispatch per half, so admitting the mix here is what makes the
    /// two sides agree.
    pub(super) fn result_field_struct_enum_payload_ok(&self, field_te: &TypeExpr) -> bool {
        let TypeKind::Path(p) = &field_te.kind else {
            return false;
        };
        if p.segments.last().map(|s| s.as_str()) != Some("Result") {
            return false;
        }
        let Some(args) = p.generic_args.as_ref() else {
            return false;
        };
        let mut half_tes = Vec::with_capacity(2);
        for a in args.iter().take(2) {
            match a {
                crate::ast::GenericArg::Type(t) => half_tes.push(t),
                _ => return false,
            }
        }
        if half_tes.len() != 2 {
            return false;
        }
        let mut any_struct_half = false;
        for half in &half_tes {
            if !self.te_owns_heap_below_buffer(half) {
                continue;
            }
            if self.option_payload_struct_or_enum_drop_ok(half) {
                any_struct_half = true;
            } else if !self.result_half_is_direct_vecstr(half) {
                return false;
            }
        }
        any_struct_half
    }

    /// Is this `Result` half a DIRECT inline-heap `String` / `Vec` / `VecDeque`
    /// the `{ptr,len,cap}` overlay copy-and-free pair handles? Shared elements
    /// are excluded for the same reason
    /// [`Self::result_field_direct_vecstr_halves_ok`] excludes them: the
    /// defensive copy emits no per-element rc-inc, so the duplicate would alias
    /// the source's boxes.
    fn result_half_is_direct_vecstr(&self, half: &TypeExpr) -> bool {
        let is_vecstr = self.is_string_type_expr(half)
            || (matches!(&half.kind, TypeKind::Path(hp)
                    if matches!(hp.segments.last().map(|s| s.as_str()), Some("Vec") | Some("VecDeque")))
                && self.extract_vec_elem_type(half).is_some());
        if !is_vecstr {
            return false;
        }
        !crate::codegen::helpers::vec_inner_type_expr(half).is_some_and(|inner| {
            matches!(&inner.kind, TypeKind::Path(ip)
                if ip.segments.last().is_some_and(|n| self.type_decls.shared_types.contains_key(n.as_str())))
        })
    }

    /// B-2026-07-21-16 — shared shape resolver for the OWNED-place
    /// `Option`/`Result` field-move legs: `value` must be a FieldAccess
    /// place whose leaf field is an `Option` with an inline non-shared
    /// String/Vec payload, or a `Result` in the direct-String/Vec-halves
    /// class, reachable through `field_chain_place_ptr` (which bails on
    /// borrowed roots — the ref-param shapes belong to the -9/-14 clone
    /// legs — and on non-place roots). Returns `(is_result, field_te,
    /// source_ptr)`. The type gates are checked BEFORE the place walk (it
    /// emits IR for `vec[i]` roots).
    pub(super) fn place_optres_field_move_info(
        &mut self,
        value: &Expr,
    ) -> Option<(bool, TypeExpr, PointerValue<'ctx>)> {
        self.place_optres_field_move_info_ex(value, false)
    }

    /// B-2026-07-28-16 — the WHOLE-MOVE variant of
    /// [`Self::place_optres_field_move_info`], for a site that hands the entire
    /// `Option`/`Result` field to a new owner (a by-value call argument). It
    /// admits a wider payload class, and the distinction is load-bearing.
    ///
    /// The narrow class exists because the PATTERN leg only zeroes the source
    /// when the arm's BINDING takes over the payload's free, which holds for an
    /// inline `{ptr,len,cap}` payload and not for a struct/enum one — zeroing
    /// there turns the owner's single free into a leak (measured: `match nd.hp
    /// { Some(x) => println(x.label) }` leaked the 4-byte payload when this
    /// class was widened in place).
    ///
    /// A whole-value move has no such question: the callee receives tag and
    /// payload together and its own cleanup frees them, so the source must be
    /// neutralized for exactly the shapes the owning struct's drop would
    /// otherwise free — which is what `emit_struct_drop_synthesis`'s
    /// `OptionInline` classifier decides.
    pub(super) fn place_optres_field_whole_move_info(
        &mut self,
        value: &Expr,
    ) -> Option<(bool, TypeExpr, PointerValue<'ctx>)> {
        self.place_optres_field_move_info_ex(value, true)
    }

    fn place_optres_field_move_info_ex(
        &mut self,
        value: &Expr,
        whole_move: bool,
    ) -> Option<(bool, TypeExpr, PointerValue<'ctx>)> {
        let ExprKind::FieldAccess { object, field } = &value.kind else {
            return None;
        };
        let obj_ty = self.place_chain_type_name(object)?;
        let idx = self
            .type_decls
            .struct_field_names
            .get(obj_ty.as_str())?
            .iter()
            .position(|n| n == field)?;
        let field_te = self
            .type_decls
            .struct_field_type_exprs
            .get(obj_ty.as_str())?
            .get(idx)?
            .clone();
        let TypeKind::Path(p) = &field_te.kind else {
            return None;
        };
        let is_result = match p.segments.last().map(|s| s.as_str()) {
            Some("Option") => {
                if self
                    .option_inner_shared_type_for_type_expr(&field_te)
                    .is_some()
                {
                    return None;
                }
                // A whole move mirrors `emit_struct_drop_synthesis`'s
                // OptionInline admission test exactly, because the two are a
                // pair: the source zero must fire precisely when the struct
                // drop would free the field, or the move becomes a double-free
                // (too narrow) or a leak (too wide). That classifier admits an
                // inline `{ptr,len,cap}` String/Vec payload AND —
                // B-2026-07-04-7 — a non-shared struct/enum payload, boxed or
                // inline. Zeroing the tag neutralizes both: the inline free
                // runs through a tag-guarded `karac_drop_Option_<H>`, and the
                // boxed `BoxedEnumDrop` guards on `tag == Some` too.
                //
                // A partial move keeps the narrow inline-only class — see
                // `place_optres_field_whole_move_info` for why they differ.
                let admitted = if whole_move {
                    Self::option_payload_te(&field_te).is_some_and(|pt| {
                        // B-2026-08-06-10 — a heap-BOXED payload is excluded, and
                        // it is the one place the pairing rule above does NOT
                        // describe the whole-move leg's only consumer. That
                        // consumer is a by-value CALL ARGUMENT
                        // (`suppress_place_optres_field_whole_move_source`), and
                        // the callee it hands the value to frees nothing when the
                        // payload is boxed: no parameter registers a box drop
                        // (caller-retains), and the arm binding is a deboxed copy
                        // that deliberately registers none either, or it would
                        // double-free against whoever owns the box. Zeroing the
                        // tag there transfers ownership to nobody and orphans the
                        // box — 32 bytes per call, measured, while the SAME callee
                        // returning a scalar stays clean only because ownership
                        // infers `ref` and this never runs.
                        //
                        // Not zeroing is only half a fix: the caller's drop would
                        // then free a buffer the callee's arm moved out. The other
                        // half is the `deboxed_payload_box_ptrs` mirror, which
                        // zeroes that field's `cap` THROUGH the box so this drop
                        // skips exactly what the move took. Pair them or neither.
                        //
                        // The INLINE classes below keep the old behaviour, where
                        // the premise does hold — an `Option[String]` payload
                        // binds to an arm binding with its own `FreeVecBuffer`.
                        if !self.option_payload_is_boxed(&pt) {
                            // B-2026-08-07-12 leg 1 — the `Map`/`Set` handle
                            // arm, added in the same commit as the classifier
                            // disjunct it mirrors. The pairing rule above is
                            // the whole reason it is here: once the struct drop
                            // frees an `Option[Map]` field, a whole-value move
                            // of that field MUST neutralize the source, or the
                            // callee's cleanup and the owner's drop free one
                            // handle twice.
                            return self.is_string_type_expr(&pt)
                                || self.extract_vec_elem_type(&pt).is_some()
                                || self.option_payload_struct_or_enum_drop_ok(&pt)
                                || self.option_payload_map_or_set_drop_ok(&pt);
                        }
                        false
                    })
                } else {
                    // B-2026-08-07-12 leg 1 — the `Map`/`Set` handle joins the
                    // NARROW class too, which is not the default for this leg
                    // and needed measuring rather than reasoning.
                    //
                    // The narrow class exists because zeroing here is only
                    // right when the ARM BINDING takes over the free, which is
                    // true of an inline `{ptr,len,cap}` payload and false of a
                    // struct/enum one (widening it wholesale is the change the
                    // doc comment above records as turning a single free into a
                    // leak). A `Map` handle behaves like the inline case, and
                    // the measurement says so directly: with the classifier
                    // widened and this leg left alone, `match s.m { Some(inner)
                    // => .. }` went from clean to 470 valgrind errors with
                    // invalid frees at BOTH opt levels — two frees, so the
                    // binding demonstrably does own it, and the source has to
                    // be neutralized.
                    self.option_inline_payload_elem(&field_te).is_some()
                        || Self::option_payload_te(&field_te)
                            .is_some_and(|pt| self.option_payload_map_or_set_drop_ok(&pt))
                };
                if !admitted {
                    return None;
                }
                false
            }
            Some("Result") => {
                // B-2026-08-03-3 leg B — mirror the struct drop's Result
                // admission exactly, same pairing rule as the Option arm
                // above: the source zero must fire precisely when the struct
                // drop would free the field. Now that the classifier also
                // admits the disjoint struct/enum-payload class, a move-out of
                // one leaked (source correctly silent, leaf never registered).
                // `zero_result_payload_area` neutralizes both widths — the
                // inline free is tag-guarded and the boxed `BoxedEnumDrop`
                // guards on a non-null box word.
                //
                // B-2026-08-07-19 — the `Map`/`Set` HANDLE half joins the same
                // admission, for the same pairing reason and on the same
                // evidence the `Option` twin above records. With the
                // classifier widened and this leg left alone, EVERY shape that
                // pattern-matches the field — `match s.s { Ok(inner) => .. }`,
                // and the whole-move / callee spellings that contain one — went
                // from clean to 470 valgrind errors with invalid frees at BOTH
                // opt levels. The `let`-move spelling stayed clean throughout,
                // because it binds the field to a local and the local's own
                // cleanup is what runs.
                //
                // `zero_result_payload_area` neutralizes the handle like any
                // other inline width: the payload area is the handle word, and
                // the struct drop's `Ok`/`Err` arm is tag-guarded.
                if !self.result_field_direct_vecstr_halves_ok(&field_te)
                    && !self.result_field_struct_enum_payload_ok(&field_te)
                    && self.result_field_map_or_set_half_ok(&field_te).is_none()
                {
                    return None;
                }
                true
            }
            _ => return None,
        };
        let src_ptr = self.field_chain_place_ptr(value)?;
        Some((is_result, field_te, src_ptr))
    }

    /// B-2026-07-21-16 (pattern leg) — a consuming variant-pattern match /
    /// if-let / let-else DIRECTLY over an owned place's `Option`/`Result`
    /// field (`match a.opt { Some(s) => … }`): the binding registers its own
    /// free (any binding sub-pattern does, even a print-only read), but
    /// nothing neutralized the SOURCE field, so the owning struct's drop
    /// (`OptionInline`) freed the same buffer again. Zero the source in the
    /// taken arm — Option TAG to `None` / Result payload area — so the
    /// struct drop skips the payload the binding now owns; non-binding arms
    /// (`Some(_)` / `None` / `Err(_)` etc.) leave the source armed and the
    /// struct drop frees it. `bindings_owned` is the scrutinee's
    /// `!pattern_binding_is_borrow` classification, captured while active —
    /// when bindings are borrow-aliases they register no free, so zeroing
    /// the source would turn the owner's single free into a leak.
    pub(super) fn suppress_consumed_place_optres_field_source(
        &mut self,
        scrutinee: &Expr,
        pattern: &Pattern,
        bindings_owned: bool,
    ) {
        if !bindings_owned {
            return;
        }
        let PatternKind::TupleVariant { path, patterns } = &pattern.kind else {
            return;
        };
        let want_result = match path.last().map(|s| s.as_str()) {
            Some("Some") => false,
            Some("Ok") | Some("Err") => true,
            _ => return,
        };
        if !patterns.iter().any(pattern_consumes_field) {
            return;
        }
        let Some((is_result, _field_te, src_ptr)) = self.place_optres_field_move_info(scrutinee)
        else {
            return;
        };
        if is_result != want_result {
            return;
        }
        if is_result {
            let Some(layout) = self.type_decls.enum_layouts.get("Result") else {
                return;
            };
            let result_ty = layout.llvm_type;
            self.zero_result_payload_area(result_ty, src_ptr, "respl.place.move");
            // B-2026-08-03-3 leg B — the memory zero above is not the full dual
            // of the Option branch's `zero_option_field_tag_at`. Zeroing only
            // the payload WORDS leaves the tag reading `Ok`/`Err`, which every
            // memory consumer tolerates (their frees are cap-guarded, so a
            // zeroed area frees nothing) but the BODIES walk does not: its
            // tag switch still selects the live arm and runs the payload's
            // `impl Drop` over the zeroed slot — a spurious `drop 0 ` after the
            // binding's correct fire. The Option side never had this because
            // its neutralizer IS the tag. Invalidate the tag so the switch
            // falls through to its default, exactly like `None`.
            self.invalidate_result_tag_at(result_ty, src_ptr, "respl.place.move");
        } else {
            self.zero_option_field_tag_at(src_ptr);
        }
    }

    /// Store a tag no `Ok`/`Err` guard can match into a moved-from `Result`
    /// slot, so BOTH the memory frees (`EQ ok_tag` / `EQ err_tag` compares) and
    /// the `emit_optres_payload_user_drop_bodies_fn` tag SWITCH fall through.
    /// The Option dual is `zero_option_field_tag_at` (tag → `None`); `Result`
    /// has no neutral variant, so `-1` stands in — it cannot collide with a
    /// real tag, which are small non-negative discriminants. Pair it with
    /// [`Self::zero_result_payload_area`]: the words carry the ownership
    /// transfer, this carries the arm selection.
    pub(super) fn invalidate_result_tag_at(
        &self,
        result_ty: inkwell::types::StructType<'ctx>,
        slot: PointerValue<'ctx>,
        name: &str,
    ) {
        let i64_t = self.context.i64_type();
        if let Ok(tag_ptr) =
            self.builder
                .build_struct_gep(result_ty, slot, 0, &format!("{name}.tag"))
        {
            let _ = self.builder.build_store(tag_ptr, i64_t.const_all_ones());
        }
    }

    /// B-2026-07-22-2 (pattern leg) — `match mk().opt { Some(s) => … }`: the
    /// scrutinee is the staged fresh-temp field access
    /// (`freshtemp_field_access_slot`), whose owning temp now carries a
    /// registered struct drop. A CONSUMING arm's binding owns the extracted
    /// payload, so zero the accessed field in the temp slot (per its kind —
    /// Option tag / Result area / Vec-String cap, via
    /// `zero_struct_field_move_cap`) in that arm; a non-binding arm leaves
    /// it armed and the temp's drop frees the field. Emitted per-arm (only
    /// the taken arm executes its zero); the channel is deliberately NOT
    /// cleared — the span-key match keeps stale entries inert for later
    /// statements.
    pub(super) fn consume_freshtemp_field_scrutinee(
        &mut self,
        scrutinee: &Expr,
        pattern: &Pattern,
        bindings_owned: bool,
    ) {
        if !bindings_owned {
            return;
        }
        let ExprKind::FieldAccess { object, field } = &scrutinee.kind else {
            return;
        };
        let Some((slot, name, ch_field, key)) = self.freshtemp_field_access_slot.clone() else {
            return;
        };
        if ch_field != *field || key != (object.span.offset, object.span.length) {
            return;
        }
        let consumes = match &pattern.kind {
            PatternKind::TupleVariant { patterns, .. } => {
                patterns.iter().any(pattern_consumes_field)
            }
            PatternKind::Struct { fields, .. } => fields
                .iter()
                .any(|f| f.pattern.as_ref().is_none_or(pattern_consumes_field)),
            PatternKind::Binding(_) => true,
            _ => false,
        };
        if !consumes {
            return;
        }
        self.zero_struct_field_move_cap(slot, &name, &ch_field);
    }

    /// B-2026-07-21-16 (let leg) — `let x = <ownedplace>.optresfield;` is a
    /// true field MOVE: register the binding's own
    /// `FreeInline{Option,Result}Payload` (so a later consuming match routes
    /// through the proven identifier-keyed suppressors) and zero the SOURCE
    /// field so the owning struct's drop skips the payload `x` now owns.
    /// Self-gating via [`Self::place_optres_field_move_info`] — borrowed
    /// roots (the -11 clone leg's territory), non-Option/Result leaves, and
    /// unsupported payload classes all no-op to the status quo.
    pub(super) fn track_place_optres_field_move(&mut self, var_name: &str, value: &Expr) {
        let Some((is_result, field_te, src_ptr)) = self.place_optres_field_move_info(value) else {
            return;
        };
        let Some(slot_ptr) = self.variables.get(var_name).map(|s| s.ptr) else {
            return;
        };
        if is_result {
            self.track_inline_result_payload_var(var_name, slot_ptr, &field_te);
            let Some(layout) = self.type_decls.enum_layouts.get("Result") else {
                return;
            };
            let result_ty = layout.llvm_type;
            self.zero_result_payload_area(result_ty, src_ptr, "respl.letmove");
        } else {
            self.track_inline_option_payload_var(var_name, slot_ptr, &field_te);
            self.zero_option_field_tag_at(src_ptr);
        }
    }

    /// B-2026-07-21-16 (assign leg) — `x = <ownedplace>.optresfield;`: zero
    /// the SOURCE only. The assign target's own cleanup registration (from
    /// its let-site) owns the moved-in payload; if the target is untracked
    /// the move can at worst leak (strictly better than the double-free),
    /// and registering here would double up on a tracked target.
    pub(super) fn suppress_place_optres_field_move_source(&mut self, value: &Expr) {
        let Some((is_result, _field_te, src_ptr)) = self.place_optres_field_move_info(value) else {
            return;
        };
        if is_result {
            let Some(layout) = self.type_decls.enum_layouts.get("Result") else {
                return;
            };
            let result_ty = layout.llvm_type;
            self.zero_result_payload_area(result_ty, src_ptr, "respl.assignmove");
        } else {
            self.zero_option_field_tag_at(src_ptr);
        }
    }

    /// B-2026-07-28-16 (call-argument leg) — `consume(nd.optresfield)` where
    /// the callee's parameter OWNS the value. Zero the source so the owning
    /// struct's scope-exit drop skips the payload the callee now frees.
    ///
    /// The sibling `suppress_inline_option_result_binding_move` covers the same
    /// move from a plain binding, and only that one was wired at the call site
    /// — it matches an `Identifier` and returns immediately for a `FieldAccess`,
    /// so `f(nd.hp)` moved the payload with nothing neutralizing the source
    /// while `let o = nd.hp; f(o)` was fine. Uses the WHOLE-move payload class
    /// (see `place_optres_field_whole_move_info`), which is wider than the
    /// pattern leg's.
    pub(super) fn suppress_place_optres_field_whole_move_source(&mut self, value: &Expr) {
        // A heap-BOXED `Option` payload is refused by the classifier's admission
        // test (B-2026-08-06-10 — see the rationale there): the caller stays
        // armed because it still owns the box, and the callee's move-out is
        // neutralized THROUGH that box instead.
        let Some((is_result, _field_te, src_ptr)) = self.place_optres_field_whole_move_info(value)
        else {
            return;
        };
        if is_result {
            let Some(layout) = self.type_decls.enum_layouts.get("Result") else {
                return;
            };
            let result_ty = layout.llvm_type;
            self.zero_result_payload_area(result_ty, src_ptr, "respl.argmove");
        } else {
            self.zero_option_field_tag_at(src_ptr);
        }
    }

    /// B-2026-08-07-7 — a match arm binds the interior out of
    /// `<local struct>.field` whose `Option` payload is heap-BOXED. Zero the
    /// field's TAG in the STRUCT's own memory so the struct's drop skips it.
    ///
    /// `struct W { o: Option[Option[String]] }` matched as
    /// `Option.Some(Option.Some(s))` aborted with a glibc double free at BOTH
    /// opt levels: `__karac_drop_struct_W` routes the field to the DEEP
    /// `karac_drop_Option_Option_String`, which frees the String inside the
    /// box, and the arm's `s` owns that same buffer.
    ///
    /// The arm cannot disarm that drop through the normal channel:
    /// `EnumDropKind::is_heap_bearing()` is false for `BoxedOptRes`, so
    /// `suppress_destructured_enum_payload_cleanup_at` skips a boxed payload BY
    /// DESIGN. Hence a second channel, writing to the struct rather than to the
    /// matched slot — the drop guards on `tag == Some` before it touches the
    /// box, so clearing the tag is sufficient and needs no knowledge of the
    /// payload's shape.
    ///
    /// FIRES ONLY WHEN AN ARM ACTUALLY CONSUMES, and that is the whole design.
    /// Making the drop box-only unconditionally was implemented and measured
    /// first: it fixed this shape and turned five passing memory_sanitizer
    /// tests into LeakSanitizer failures, every one an UNDESTRUCTURED or
    /// BORROW-ONLY shape whose interior the deep drop legitimately owns. Gating
    /// on consumption leaves all five untouched by construction.
    ///
    /// COST: the 32-byte envelope leaks in the consuming case, because zeroing
    /// the tag also skips the box free and nothing else claims it — the same
    /// trade `place_optres_field_move_info_ex` refuses for the CALL-ARGUMENT
    /// leg, where a leak would be a regression. Here the alternative is
    /// corruption, so it is the right way round.
    ///
    /// PLACE ROOTS ONLY: a `ref`/borrowed or shared root is refused via
    /// `field_chain_place_ptr`, which already declines those for the reason it
    /// gives — writing through a borrow is corruption, not neutralization.
    pub(super) fn suppress_struct_field_boxed_payload_match_out(
        &mut self,
        scrutinee: &Expr,
        pattern: &Pattern,
        arm_body: &Expr,
    ) {
        if self.pattern_state.pattern_binding_is_borrow {
            return;
        }
        let ExprKind::FieldAccess { object, field } = &scrutinee.kind else {
            return;
        };
        // Only a sub-pattern that DESTRUCTURES INTO the payload — a nested
        // variant pattern that itself binds — and never a plain binding of the
        // whole payload.
        //
        // That distinction is the entire gate, and it was measured rather than
        // reasoned. `match a.value { Some(v) => … }` binds the WHOLE payload
        // out; `v` owns the payload and the struct's drop still owns the box,
        // which is a correct division of one allocation each — zeroing the tag
        // there orphans the box and its interior (measured: 504 B over 14
        // allocations in `asan_b04_7_option_heap_enum_struct_field_drop`, an
        // -O0-leg failure). `Some(Some(s))` is the broken one: the arm claims
        // the INTERIOR while the deep drop frees it too, and neither owns the
        // box exclusively.
        //
        // A wildcard or literal sub-pattern consumes nothing and leaves the
        // interior to the struct's drop — the population the box-only attempt
        // broke, excluded here by the same test.
        //
        // B-2026-08-07-9 — a plain binding qualifies too, but ONLY when the arm
        // body goes on to destructure it. `match w.o { Some(inner) => match
        // inner { Some(s) => … } }` reaches the same interior by two steps
        // instead of one and double-freed identically; the `let inner = w.o;`
        // spelling of it is clean because the let-move runs its own source
        // suppression, which is precisely what the arm binding lacks.
        //
        // Looking at the BODY is what separates it from `asan_b04_7`'s
        // `Some(v) => ident_len(v)`, which is correct today and must stay
        // untouched. At the arm both are `Some(<binding>)` and nothing about
        // the pattern tells them apart — what differs is what happens to the
        // binding next, so that is what this asks.
        let consumes = match &pattern.kind {
            PatternKind::TupleVariant { patterns, .. } => patterns.iter().any(|sub| {
                if matches!(
                    &sub.kind,
                    PatternKind::TupleVariant { .. } | PatternKind::Struct { .. }
                ) {
                    return pattern_consumes_field(sub);
                }
                match &sub.kind {
                    PatternKind::Binding(n) => Self::expr_destructures_binding(arm_body, n),
                    _ => false,
                }
            }),
            _ => false,
        };
        if !consumes {
            return;
        }
        let Some(obj_ty) = self.place_chain_type_name(object) else {
            return;
        };
        let Some(idx) = self
            .type_decls
            .struct_field_names
            .get(obj_ty.as_str())
            .and_then(|ns| ns.iter().position(|n| n == field))
        else {
            return;
        };
        let Some(field_te) = self
            .type_decls
            .struct_field_type_exprs
            .get(obj_ty.as_str())
            .and_then(|tes| tes.get(idx))
            .cloned()
        else {
            return;
        };
        // The struct's drop must actually be the rival: an `Option` field whose
        // payload is BOXED and heap-owning is the only shape routed to the deep
        // drop with no suppression channel. An inline payload keeps the
        // existing machinery, which already works.
        if self
            .option_inner_shared_type_for_type_expr(&field_te)
            .is_some()
        {
            return;
        }
        let Some(pt) = Self::option_payload_te(&field_te) else {
            return;
        };
        if !self.option_payload_is_boxed(&pt) || !self.option_payload_struct_or_enum_drop_ok(&pt) {
            return;
        }
        if let Some(field_ptr) = self.field_chain_place_ptr(scrutinee) {
            // Order matters: park the box pointer BEFORE the tag zero, which is
            // what takes the field's drop — and with it the envelope free — off
            // the table.
            // B-2026-08-07-11 residue — the box this parks may itself hold
            // further ENVELOPES (`Option[Option[Option[String]]]` as a field:
            // the field's payload boxes, and so does the payload inside that
            // box). Freeing only the parked one leaked 32 B per match.
            //
            // A FIRST owner here, not a second, and that is measured rather
            // than argued: the field's own drop is about to be disarmed by the
            // tag zero below, the arm's binding owns the INTERIOR only, and the
            // pre-fix result was a LEAK — had anything else claimed these
            // envelopes it would have been a double free. The comment this
            // replaces deferred them to B-2026-08-07-6/-11 while those were in
            // flight; all three of their legs have since landed and none of
            // them reaches this path.
            let deeper = self.nested_box_deeper_tag_chain(&pt);
            self.own_boxed_option_field_envelope_at(field_ptr, deeper);
            self.zero_option_field_tag_at(field_ptr);
        }
    }

    /// Does `e` contain a `match <name> { … }` (or `if let … = <name>`) whose
    /// pattern destructures INTO the payload rather than binding it whole?
    /// B-2026-08-07-9.
    ///
    /// Syntactic on purpose. The alternative is alias tracking from an arm
    /// binding to the struct field it came from, and the question here is
    /// narrower than that: this only needs to know whether the binding's own
    /// interior gets claimed by a second destructure, which is visible in the
    /// arm body. A conservative FALSE just leaves the pre-existing double free
    /// alone; a conservative TRUE would orphan an envelope, so the walk matches
    /// the same "nested variant sub-pattern" shape the direct case requires
    /// rather than any use of the name.
    fn expr_destructures_binding(e: &Expr, name: &str) -> bool {
        match &e.kind {
            ExprKind::Match { scrutinee, arms } => {
                if let ExprKind::Identifier(n) = &scrutinee.kind {
                    if n == name
                        && arms.iter().any(|a| match &a.pattern.kind {
                            PatternKind::TupleVariant { patterns, .. } => {
                                patterns.iter().any(pattern_consumes_field)
                            }
                            _ => false,
                        })
                    {
                        return true;
                    }
                }
                arms.iter()
                    .any(|a| Self::expr_destructures_binding(&a.body, name))
            }
            // A block body reaches the same match one level in; the tail is the
            // common spelling and a statement position the rest.
            ExprKind::Block(b) => {
                b.final_expr
                    .as_deref()
                    .is_some_and(|t| Self::expr_destructures_binding(t, name))
                    || b.stmts.iter().any(|st| match &st.kind {
                        crate::ast::StmtKind::Expr(x) => Self::expr_destructures_binding(x, name),
                        crate::ast::StmtKind::Let { value, .. } => {
                            Self::expr_destructures_binding(value, name)
                        }
                        _ => false,
                    })
            }
            _ => false,
        }
    }

    /// B-2026-08-07-7 residue — give the payload ENVELOPE an owner, so
    /// disarming the field's drop above stops double-freeing the interior
    /// without also orphaning the 32-byte box that held it.
    ///
    /// `zero_option_field_tag_at` is a single guard away from two different
    /// frees. `karac_drop_Option_<P>` tests `tag == Some` before it touches
    /// anything, and behind that one test it does BOTH the deep drop of the
    /// interior (the double free this row fixed) and the `free` of the box
    /// itself (which nothing else claims). Zeroing the tag is the only channel
    /// the arm has — a boxed payload has no `is_heap_bearing` suppression path
    /// — so it necessarily takes the envelope with it.
    ///
    /// The recovery is to park the box pointer in a private slot the struct's
    /// drop cannot see, and register a BOX-ONLY `BoxedEnumDrop` (`inner_drop_fn:
    /// None`) against it. Interior ownership is unchanged — the arm's binding
    /// still owns it, which is the whole point of the tag zero.
    ///
    /// DEFERRED TO THE ARM'S FRAME, NOT FREED HERE, and the distinction is
    /// load-bearing rather than stylistic. `scrutinee_is_owned_param_binding`
    /// walks THROUGH field access, so `match w.o` inside a fn taking `w: W` by
    /// value sets `pattern_binding_scrutinee_is_owned_param` — which arms
    /// B-2026-08-06-10's `deboxed_payload_box_ptrs` mirror, whose whole job is
    /// to WRITE neutralizing zeroes into this same box at a later move site in
    /// the arm's body. An eager free here would turn each of those writes into
    /// a use-after-free. Riding the arm's cleanup frame puts the free after the
    /// body, and pushing it before the body appends means LIFO drains it last —
    /// after the binding that owns the interior.
    ///
    /// The parked tag is defaulted to `None` in the entry block, so a drain on
    /// a path that never reached this arm (an arm GUARD that fails after the
    /// frame is pushed) reads a slot that frees nothing rather than garbage.
    fn own_boxed_option_field_envelope_at(
        &mut self,
        field_ptr: PointerValue<'ctx>,
        deeper_tags: Vec<u64>,
    ) {
        let Some(fn_val) = self.current_fn else {
            return;
        };
        let Some(layout) = self.type_decls.enum_layouts.get("Option").cloned() else {
            return;
        };
        let (Some(some_tag), Some(none_tag)) = (
            layout.tags.get("Some").copied(),
            layout.tags.get("None").copied(),
        ) else {
            return;
        };
        if some_tag == none_tag {
            return;
        }
        let i64_t = self.context.i64_type();
        let Ok(w0_ptr) =
            self.builder
                .build_struct_gep(layout.llvm_type, field_ptr, 1, "boxenv.src.ptr")
        else {
            return;
        };
        let Ok(w0) = self.builder.build_load(i64_t, w0_ptr, "boxenv.src") else {
            return;
        };
        let Some(entry) = fn_val.get_first_basic_block() else {
            return;
        };
        let slot_ty = self
            .context
            .struct_type(&[i64_t.into(), i64_t.into()], false);
        let alloca_b = self.context.create_builder();
        match entry.get_first_instruction() {
            Some(first) => alloca_b.position_before(&first),
            None => alloca_b.position_at_end(entry),
        }
        let Ok(slot) = alloca_b.build_alloca(slot_ty, "boxenv.slot") else {
            return;
        };
        let init_b = self.context.create_builder();
        match entry.get_terminator() {
            Some(term) => init_b.position_before(&term),
            None => init_b.position_at_end(entry),
        }
        if let Ok(tag_ptr) = init_b.build_struct_gep(slot_ty, slot, 0, "boxenv.init.tag") {
            let _ = init_b.build_store(tag_ptr, i64_t.const_int(none_tag, false));
        }
        if let Ok(tag_ptr) = self
            .builder
            .build_struct_gep(slot_ty, slot, 0, "boxenv.arm.tag")
        {
            let _ = self
                .builder
                .build_store(tag_ptr, i64_t.const_int(some_tag, false));
        }
        if let Ok(box_ptr) = self
            .builder
            .build_struct_gep(slot_ty, slot, 1, "boxenv.arm.w0")
        {
            let _ = self.builder.build_store(box_ptr, w0);
        }
        if let Some(frame) = self.drop_rc.scope_cleanup_actions.last_mut() {
            frame.push(super::state::CleanupAction::BoxedEnumDrop {
                // Not a Kāra identifier — the name keys `clear_boxed_enum_
                // inner_drop` / `suppress_*_for_var`, and this action must never
                // answer to a user binding's retraction.
                name: "boxenv".to_string(),
                enum_slot: slot,
                enum_ty: slot_ty,
                inner_drop_fn: None,
                some_tag,
                // The envelopes below the parked one — see the call site for
                // why claiming them here is a FIRST owner. Empty for the
                // single-box shape, which is every field whose payload does not
                // box again, and the walk then collapses to the same lone
                // `free` this action emitted before.
                deeper_tags,
            });
        }
    }

    /// B-2026-08-06-10 — record the enum payload BOX a just-bound match-arm
    /// binding was deboxed out of, so the move-out neutralizers can mirror
    /// their zero into storage the box's owner can see. Call sites pair this
    /// with [`Self::reconstruct_payload_value`] + `bind_pattern_values`, and
    /// pass the SAME `field_words`, because the debox predicate here has to be
    /// the same one the reconstruction used.
    ///
    /// Insert-or-REMOVE, never insert-only: two arms can bind the same name
    /// with only one of them boxed, and a stale entry would point the mirror at
    /// an unrelated allocation. Keying by slot already makes that unreachable;
    /// the removal keeps the map honest anyway, at the cost of one hash lookup.
    pub(super) fn record_deboxed_payload_box(
        &mut self,
        sub_pat: &Pattern,
        field_words: &[inkwell::values::IntValue<'ctx>],
    ) {
        let PatternKind::Binding(name) = &sub_pat.kind else {
            return;
        };
        let Some(slot) = self.variables.get(name.as_str()).map(|s| s.ptr) else {
            return;
        };
        // OWNED-PARAM scrutinees are the shape where the box's owner is out of
        // reach entirely, so they always record. An in-frame owner is a
        // `BoxedEnumDrop` in this frame's cleanup queue, and B-2026-08-04-2
        // neutralizes those by RETRACTING the action's inner walk — which it
        // chose over a data write for a reason worth not re-learning: the box's
        // words are also what a re-homed payload-BODIES walk reads, and zeroing
        // them made a user Drop body print an empty string, a double free
        // traded for a silent wrong value.
        //
        // B-2026-08-17-45 — retraction is whole-action, so it covers the WHOLE
        // payload escaping and nothing else. Move a single `Option`-typed FIELD
        // out of the arm binding (`match a { Some(x) => x.c }`) and there is no
        // action to retract: the box's drop must still run for every field the
        // move did NOT take. The neutralizer that exists for exactly this —
        // `suppress_source_vec_cleanup_for_arg_ex`'s per-field cap/tag zero —
        // writes into the DEBOXED COPY, which the box's owner cannot see, so
        // `__karac_drop_struct_A(box)` freed the field the move had already
        // handed away: `free(): double free detected in tcache 2` under both
        // compiled backends on a program `--interp` runs correctly.
        //
        // So record for an in-frame owner too, but ONLY when no payload-BODIES
        // walk rides this box — which is precisely the hazard the paragraph
        // above measured, and precisely what
        // `pattern_binding_scrutinee_payload_bodies_src` already tracks (it is
        // `Some` only when the payload runs a user Drop). With it `None` the
        // box's only reader is a compiler-generated MEMORY drop, and a
        // per-field zero through the box is neutralization rather than
        // corruption: the mirror writes one field, so every field the move left
        // alone keeps its live cap and that drop still frees it. With it
        // `Some`, nothing is recorded and the shape stays byte-identical.
        //
        // B-2026-08-18-4 — with a bodies walk present the box is not recorded
        // for the IMMEDIATE mirror, but it is no longer dropped on the floor
        // either: it goes on the DEFERRED channel instead, so a per-field zero
        // can be emitted BETWEEN the two readers (after the bodies walk, before
        // the box's memory drop) rather than ahead of both. That keeps the
        // measurement above intact — the body still reads live fields — while
        // closing the double free it left open. `defer_box` is threaded to the
        // recording tail so both channels share one box pointer derivation.
        let defer_box = !self.pattern_state.pattern_binding_scrutinee_is_owned_param
            && self
                .pattern_state
                .pattern_binding_scrutinee_payload_bodies_src
                .is_some();
        if defer_box {
            self.payload_vars.deboxed_payload_box_ptrs.remove(&slot);
        }
        // …and never through a SHARED enum's payload box. That box lives inside
        // an RC node other handles can still read, so a cap/len zero there is
        // corruption rather than neutralization — the same reason
        // `field_chain_place_ptr` refuses a borrowed root, and the reason
        // B-2026-07-28-17 hands the callee a defensive CLONE instead. That clone
        // is a private non-shared `Option`, so the shape this row measured
        // still reaches the mirror; only the shared original is off limits.
        if self.pattern_state.pattern_binding_scrutinee_is_shared_enum {
            self.payload_vars.deboxed_payload_box_ptrs.remove(&slot);
            self.payload_vars.deferred_payload_box_ptrs.remove(name);
            return;
        }
        // The unpack mirror of `coerce_to_payload_words`'s pack-side boxing —
        // identical to `reconstruct_payload_value`'s own `want > field_words`.
        if field_words.is_empty() || self.pattern_payload_word_count(sub_pat) <= field_words.len() {
            self.payload_vars.deboxed_payload_box_ptrs.remove(&slot);
            self.payload_vars.deferred_payload_box_ptrs.remove(name);
            return;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        match self
            .builder
            .build_int_to_ptr(field_words[0], ptr_ty, "enumbox.mv")
        {
            Ok(box_ptr) => {
                if defer_box {
                    // Deferred channel (B-2026-08-18-4): keyed by NAME, because
                    // the drain site is the `CleanupAction::UserDrop` arm and
                    // the name is what that action carries.
                    self.payload_vars
                        .deferred_payload_box_ptrs
                        .insert(name.clone(), box_ptr);
                } else {
                    self.payload_vars
                        .deboxed_payload_box_ptrs
                        .insert(slot, box_ptr);
                }
            }
            Err(_) => {
                self.payload_vars.deboxed_payload_box_ptrs.remove(&slot);
                self.payload_vars.deferred_payload_box_ptrs.remove(name);
            }
        }
    }

    /// `if let` / `while let` sibling of the arm-loop escape walk in
    /// [`Self::no_arm_payload_escapes`]: does the given BLOCK move any of the
    /// pattern's leaf bindings? Feeds the `escapes` flag of the ref-chain
    /// clone legs from the single-pattern binding sites (B-2026-07-21-8).
    pub(super) fn pattern_bindings_escape_in_block(
        &self,
        pattern: &Pattern,
        block: &Block,
    ) -> bool {
        let mut names: Vec<String> = Vec::new();
        collect_pattern_bindings(pattern, &mut names);
        names
            .iter()
            .any(|n| self.borrow_binding_escapes_block(block, n))
    }

    /// A whole-pattern `Binding(n)` that resolves to a unit VARIANT is a tag
    /// test, not a binding — the parser has no separate node for `None` /
    /// `Color.Red` in pattern position and hands both back as `Binding`.
    /// `compile_pattern_condition` disambiguates them with exactly this lookup
    /// pair before deciding to emit a tag compare instead of a bind.
    ///
    /// B-2026-08-08-25 leg 1: without this, the escape guard collected `None`
    /// as a payload binding, and any arm body that MENTIONS `None` — `None =>
    /// None`, the single most ordinary shape there is — read as "that binding
    /// escapes", vetoing the read-only classification for the whole match. It
    /// is what kept `.map` on the transfer path (its synthesis emits precisely
    /// `None => None`), and it equally broke the hand-written
    /// `match o { Some(v) => Some(v.len()), None => None }`.
    fn pattern_is_unit_variant_test(&self, p: &Pattern) -> bool {
        let PatternKind::Binding(name) = &p.kind else {
            return false;
        };
        let variant_name = name.rsplit('.').next().unwrap_or(name);
        self.variant_pattern_enum_and_tag(p).is_some()
            || self.enum_tag_for_variant(variant_name).is_some()
    }

    /// No arm moves any of its pattern's leaf bindings out of the arm body or
    /// guard (the escape guard shared by the read-only borrow classifications).
    fn no_arm_payload_escapes(&self, arms: &[MatchArm]) -> bool {
        for arm in arms {
            let mut names: Vec<String> = Vec::new();
            collect_pattern_bindings(&arm.pattern, &mut names);
            // Not a binding at all — nothing to escape. Clearing rather than
            // skipping the arm keeps the guard's shape: the arm's body is still
            // walked for every OTHER arm's bindings by the outer loop.
            if self.pattern_is_unit_variant_test(&arm.pattern) {
                names.clear();
            }
            for name in &names {
                if self.borrow_binding_escapes(&arm.body, name) {
                    return false;
                }
                if let Some(guard) = &arm.guard {
                    if self.borrow_binding_escapes(guard, name) {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// Walk a place expression (`a.b.c`, `a[i].b`, `a.0`) down to its root
    /// `Identifier` / `self` sub-expression, returning it — or `None` if the
    /// chain bottoms out at a non-place root (a call, a literal, etc.).
    fn place_expr_root_ident<'e>(&self, expr: &'e Expr) -> Option<&'e Expr> {
        match &expr.kind {
            ExprKind::Identifier(_) | ExprKind::SelfValue => Some(expr),
            ExprKind::FieldAccess { object, .. }
            | ExprKind::Index { object, .. }
            | ExprKind::TupleIndex { object, .. } => self.place_expr_root_ident(object),
            _ => None,
        }
    }

    pub(super) fn scrutinee_is_borrow_call(&self, scrutinee: &Expr) -> bool {
        let ExprKind::MethodCall { object, method, .. } = &scrutinee.kind else {
            return false;
        };
        // `get`/`first`/`last` on a Vec/Slice now return `Option[ref T]` — the
        // payload aliases the container's element storage, so a `Some(x)`
        // binding must NOT register its own buffer cleanup (double-free against
        // the container's drop). `first`/`last` were previously omitted because
        // the v1 stdlib only returned scalar payloads from them; with the
        // `Option[ref T]` flip a heap-bearing element (`Vec[String].first()`)
        // can reach here, so they need the same borrow classification as `get`.
        if !matches!(method.as_str(), "get" | "first" | "last") {
            return false;
        }
        // `Client.get(url)` is a GET request returning a freshly-**owned**
        // `Result[Response, HttpError]` — opposite ownership from a collection
        // accessor; suppressing its cleanup leaks the response (B-2026-06-10-3).
        // `first`/`last` have no `Client` overload, so the guard only bites
        // `get`, but applying it uniformly is harmless and future-proof.
        !matches!(
            self.inferred_receiver_type(object).as_deref(),
            Some("Client")
        )
    }

    /// Slice 3s (B-2026-07-01-12): the payload TypeExpr of a VALUE-typed
    /// borrow-call scrutinee whose bound-out payload must be DEEP-CLONED
    /// when the arm body moves it. `Map.get(k)` is the class: its result is
    /// `Option[V]` (value-typed — the typechecker blesses a move-out), but
    /// codegen binds the payload as an ALIAS of the bucket's value; an
    /// escaping binding hands that alias to an owner that frees it, double-
    /// freeing against the map's stored-value drop (`drop_val` walk or the
    /// 3r per-value drop fn). `Vec`/`Slice` `get`/`first`/`last` return
    /// `Option[ref T]` and the typechecker REJECTS escapes, so their `te`
    /// here is a `Ref` (non-`Path` arg) and they self-gate to `None`.
    ///
    /// Returns `Some(payload_te)` only when the payload owns heap, is not
    /// shared (the rc-inc path owns those), and the clone/drop family fully
    /// supports the shape — anything else keeps the status-quo alias (a
    /// pre-existing, narrower leak/double-free is never made worse by a
    /// partial clone).
    pub(super) fn borrow_get_payload_clone_te(&mut self, scrutinee: &Expr) -> Option<TypeExpr> {
        // A `let g = m.get(k)` binding re-enters here as an IDENTIFIER scrutinee
        // (`match g { … }` / `if let Some(v) = g`); its recorded `Option[V]`
        // type comes from `borrow_accessor_let_payload`, not the span-keyed
        // `enum_inst_type_exprs` (which keys on the get-CALL span, not the
        // identifier use). B-2026-07-09-13.
        let te = if let ExprKind::Identifier(name) = &scrutinee.kind {
            self.borrow_vars
                .borrow_accessor_let_payload
                .get(name)?
                .clone()
        } else {
            if !self.scrutinee_is_borrow_call(scrutinee) {
                return None;
            }
            // Only `get` — `first`/`last` have no Map overload, and their Vec
            // forms are ref-typed (rejected escapes).
            let ExprKind::MethodCall { method, .. } = &scrutinee.kind else {
                return None;
            };
            if method != "get" {
                return None;
            }
            let key = (scrutinee.span.offset, scrutinee.span.length);
            self.type_decls.enum_inst_type_exprs.get(&key)?.clone()
        };
        let TypeKind::Path(p) = &te.kind else {
            return None;
        };
        if p.segments.last().map(|s| s.as_str()) != Some("Option") {
            return None;
        }
        let GenericArg::Type(payload_te) = p.generic_args.as_ref()?.first()? else {
            return None;
        };
        let payload_te = payload_te.clone();
        self.borrow_payload_clone_te_gate(&payload_te)
    }

    /// The payload-shape gate of [`Self::borrow_get_payload_clone_te`], split
    /// out so a caller that already HAS the payload `TypeExpr` can reuse the
    /// identical safety rules instead of restating them (B-2026-08-11-11).
    ///
    /// `unwrap_or` is that caller: it knows its own `inner_te`, while the
    /// span-keyed `enum_inst_type_exprs` lookup above has no entry at a
    /// non-scrutinee use site — so the receiver reads as a borrow call and the
    /// payload type still came back `None`, which is why the double-free
    /// survived a first fix attempt that only reused the whole predicate.
    pub(super) fn borrow_payload_clone_te_gate(
        &mut self,
        payload_te: &TypeExpr,
    ) -> Option<TypeExpr> {
        let payload_te = payload_te.clone();
        // Slice 3u: a TUPLE payload passes through whole — the fixup's
        // Tuple-pattern arm gates each ELEMENT individually. A `ref`-typed
        // payload (Vec accessors) still self-gates to None below.
        if matches!(payload_te.kind, TypeKind::Tuple(_)) {
            return Some(payload_te);
        }
        let TypeKind::Path(pp) = &payload_te.kind else {
            return None;
        };
        let head = pp.segments.first()?.as_str();
        if !self.te_owns_heap_below_buffer(&payload_te) {
            return None;
        }
        if self.shared_heap_type_for_type_expr(&payload_te).is_some() {
            return None;
        }
        let supported = match head {
            "String" | "str" => true,
            // NOT `VecDeque` — the clone dispatcher has no VecDeque arm, so
            // it would fall through to the SHALLOW primitive clone (an
            // alias, worse than the status quo).
            "Vec" | "Map" | "Set" => self.te_recursive_drop_fully_supported(&payload_te),
            _ => {
                (self.type_decls.struct_types.contains_key(head)
                    || self.type_decls.enum_layouts.contains_key(head))
                    && !self.type_decls.shared_types.contains_key(head)
                    && self.te_recursive_drop_fully_supported(&payload_te)
            }
        };
        supported.then_some(payload_te)
    }

    /// Slice 3s fixup: after a borrow-mode `Some(x)` bind over a `Map.get`
    /// scrutinee, when the arm (guard + body for `match`, the given block
    /// for `if let`/`while let`, unconditionally for `let…else`) MOVES the
    /// bound name, replace the alias in `x`'s slot with a DEEP CLONE and
    /// register normal owned tracking — from then on `x` is
    /// indistinguishable from an owned binding, so every existing move-out
    /// suppression (arm-tail identity, push/insert source-zeroing, tail
    /// return) applies unchanged, and a non-moved clone is freed by the
    /// scope frame's drain. Read-only arms never reach the clone (the
    /// escape walk returns false), keeping the hot `match m.get(k) {
    /// Some(c) => …read… }` shape zero-cost. `escape_scope`: `Some(exprs)`
    /// → analyze those; `None` → the binding outlives analysis reach
    /// (`let…else`), always clone.
    pub(super) fn clone_escaping_borrow_payload_binding(
        &mut self,
        scrutinee: &Expr,
        pattern: &Pattern,
        escape_exprs: Option<&[&Expr]>,
        escape_blocks: &[&Block],
    ) -> Result<(), String> {
        let Some(payload_te) = self.borrow_get_payload_clone_te(scrutinee) else {
            return Ok(());
        };
        // Whole-payload `Some(x)` binding only — destructuring payloads
        // (`Some((a, b))`, `Some(H { f })`) keep the status-quo alias and
        // stay with B-2026-07-01-12's residual note.
        let PatternKind::TupleVariant {
            path,
            patterns: subs,
        } = &pattern.kind
        else {
            return Ok(());
        };
        if path.last().map(|s| s.as_str()) != Some("Some") || subs.len() != 1 {
            return Ok(());
        }
        match &subs[0].kind {
            PatternKind::Binding(name) => {
                let name = name.clone();
                if self.borrow_binding_escape_check(&name, escape_exprs, escape_blocks) {
                    self.clone_and_track_borrow_binding(&name, &payload_te);
                }
            }
            // Slice 3t: struct-DESTRUCTURE of the borrowed payload
            // (`Some(Holder { name, .. }) => name` over `m.get(k)`) — each
            // escaping FIELD binding aliases the bucket's field buffer and
            // needs its own clone + owned tracking, at field granularity
            // (a read-only field stays a zero-cost alias).
            PatternKind::Struct {
                path: spath,
                fields,
                ..
            } => {
                let Some(struct_name) = spath.last().cloned() else {
                    return Ok(());
                };
                let Some(field_names) = self
                    .type_decls
                    .struct_field_names
                    .get(&struct_name)
                    .cloned()
                else {
                    return Ok(());
                };
                let field_tes = self
                    .type_decls
                    .struct_field_type_exprs
                    .get(&struct_name)
                    .cloned()
                    .unwrap_or_default();
                for field_pat in fields {
                    // The binding name: shorthand (`{ name }`) or a direct
                    // sub-Binding (`{ name: n }`). Deeper sub-patterns keep
                    // the status-quo alias.
                    let bind_name = match field_pat.pattern.as_ref().map(|p| &p.kind) {
                        None => field_pat.name.clone(),
                        Some(PatternKind::Binding(n)) => n.clone(),
                        _ => continue,
                    };
                    let Some(idx) = field_names.iter().position(|n| n == &field_pat.name) else {
                        continue;
                    };
                    let Some(field_te) = field_tes.get(idx).cloned() else {
                        continue;
                    };
                    if !self.borrow_payload_clone_supported(&field_te) {
                        continue;
                    }
                    if self.borrow_binding_escape_check(&bind_name, escape_exprs, escape_blocks) {
                        self.clone_and_track_borrow_binding(&bind_name, &field_te);
                    }
                }
            }
            // Slice 3u: TUPLE destructure of the borrowed payload
            // (`Some((a, b)) => a` over `m.get(k)`) — clone each ESCAPING
            // heap element; read-only elements stay zero-cost aliases.
            PatternKind::Tuple(elems) => {
                let TypeKind::Tuple(elem_tes) = &payload_te.kind else {
                    return Ok(());
                };
                let pairs: Vec<(String, TypeExpr)> = elems
                    .iter()
                    .zip(elem_tes.iter())
                    .filter_map(|(ep, ete)| match &ep.kind {
                        PatternKind::Binding(n) => Some((n.clone(), ete.clone())),
                        _ => None,
                    })
                    .collect();
                for (bind_name, elem_te) in pairs {
                    if !self.borrow_payload_clone_supported(&elem_te) {
                        continue;
                    }
                    if self.borrow_binding_escape_check(&bind_name, escape_exprs, escape_blocks) {
                        self.clone_and_track_borrow_binding(&bind_name, &elem_te);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Whether the arm scope moves `name` (see `borrow_binding_escapes`).
    /// `escape_exprs = None` → the binding outlives analysis reach
    /// (`let…else`) — always treat as escaping.
    fn borrow_binding_escape_check(
        &self,
        name: &str,
        escape_exprs: Option<&[&Expr]>,
        escape_blocks: &[&Block],
    ) -> bool {
        match escape_exprs {
            None => true,
            Some(exprs) => {
                exprs.iter().any(|e| self.borrow_binding_escapes(e, name))
                    || escape_blocks
                        .iter()
                        .any(|b| self.borrow_binding_escapes_block(b, name))
            }
        }
    }

    /// The clone-family support gate shared by the whole-payload and
    /// destructured-field legs — a `false` keeps the status-quo alias
    /// (never a partial clone).
    fn borrow_payload_clone_supported(&mut self, te: &TypeExpr) -> bool {
        // Whole-tuple payloads (slice 3v): supported when every heap element
        // is (the tuple clone/drop synthesizers recurse per element).
        if let TypeKind::Tuple(elems) = &te.kind {
            return elems.iter().all(|e| {
                !self.te_owns_heap_below_buffer(e) || self.borrow_payload_clone_supported(e)
            });
        }
        let TypeKind::Path(pp) = &te.kind else {
            return false;
        };
        let Some(head) = pp.segments.first().map(|s| s.as_str()) else {
            return false;
        };
        if !self.te_owns_heap_below_buffer(te) {
            return false;
        }
        if self.shared_heap_type_for_type_expr(te).is_some() {
            return false;
        }
        match head {
            "String" | "str" => true,
            // Slice 3v: VecDeque re-admitted — it shares Vec's linear
            // {ptr,len,cap} layout (push_front is a memmove insert at index
            // 0), and the clone/drop dispatchers now route it through the
            // Vec arms.
            "Vec" | "VecDeque" | "Map" | "Set" => self.te_recursive_drop_fully_supported(te),
            _ => {
                (self.type_decls.struct_types.contains_key(head)
                    || self.type_decls.enum_layouts.contains_key(head))
                    && !self.type_decls.shared_types.contains_key(head)
                    && self.te_recursive_drop_fully_supported(te)
            }
        }
    }

    /// Deep-clone the binding's slot in place and register owned tracking —
    /// the escaping-borrow fixup tail shared by the whole-payload and
    /// destructured-field legs. After this the binding is indistinguishable
    /// from an owned one, so every existing move-out suppression applies.
    fn clone_and_track_borrow_binding(&mut self, name: &str, te: &TypeExpr) {
        let Some(slot) = self.variables.get(name).copied() else {
            return;
        };
        let Some(fn_val) = self.current_fn else {
            return;
        };
        let clone_fn = self.emit_clone_fn_for_type_expr(te);
        let tmp = self.create_entry_alloca(fn_val, "borrow.clone.tmp", slot.ty);
        self.builder
            .build_call(clone_fn, &[slot.ptr.into(), tmp.into()], "")
            .unwrap();
        let cloned = self
            .builder
            .build_load(slot.ty, tmp, "borrow.clone.v")
            .unwrap();
        self.builder.build_store(slot.ptr, cloned).unwrap();
        // Whole-TUPLE binding (slice 3v): track the cloned tuple via the
        // per-element te drop synthesis — a NON-tail consume (`f(x)`
        // by-value, where the callee copies) would otherwise leak the clone
        // (the tail-move suppressions no-op an already-suppressed drop, so
        // tracking is safe for the tail path too).
        if let TypeKind::Tuple(elem_tes) = &te.kind {
            if let BasicTypeEnum::StructType(agg_ty) = slot.ty {
                let elem_tes = elem_tes.clone();
                if let Some(drop_fn) = self.synthesize_tuple_drop_fn_te(agg_ty, &elem_tes) {
                    if let Some(frame) = self.drop_rc.scope_cleanup_actions.last_mut() {
                        frame.push(crate::codegen::state::CleanupAction::StructDrop {
                            struct_alloca: slot.ptr,
                            drop_fn,
                        });
                    }
                }
            }
            return;
        }
        let TypeKind::Path(pp) = &te.kind else {
            return;
        };
        let head = pp.segments.first().map(|s| s.as_str()).unwrap_or("");
        match head {
            "String" | "str" | "Vec" | "VecDeque" => {
                let elem_ty = self.inline_heap_payload_elem(te);
                self.track_vec_var(slot.ptr, elem_ty);
            }
            "Map" | "Set" => {
                let (key_is_vec, val_is_vec, key_shared, val_shared, val_drop_fn, key_drop_fn) =
                    self.map_temp_cleanup_parts(te);
                self.track_map_var_with_val_drop(
                    slot.ptr,
                    key_is_vec,
                    val_is_vec,
                    val_shared,
                    key_shared,
                    val_drop_fn,
                    key_drop_fn,
                );
            }
            _ => {
                if self.type_decls.struct_types.contains_key(head) {
                    let head = head.to_string();
                    self.track_struct_var(&head, slot.ptr);
                } else if self.type_decls.enum_layouts.contains_key(head) {
                    let head = head.to_string();
                    self.track_enum_var(&head, slot.ptr);
                }
            }
        }
    }

    /// Conservative escape analysis for a borrow-mode payload binding: does
    /// `name` occur in a MOVE position anywhere in `e`? Borrow positions —
    /// where a BARE `name` does not count — are: method receiver, field /
    /// tuple-index / index base, binary/unary operands, f-string
    /// interpolations, `println`-family call args, cast operands, and index
    /// keys. EVERYTHING else that mentions the name (call/method args, arm
    /// or block tails, `return`/`break` values, let/assign RHS, composite
    /// literals, closures, `match` scrutinees, …) counts as a move — a
    /// false positive only costs one clone; a false negative is a
    /// double-free, so unrecognized shapes must land on the move side. The
    /// match is EXHAUSTIVE over `ExprKind` on purpose: a future variant
    /// fails the build here and forces a classification.
    pub(super) fn borrow_binding_escapes(&self, e: &Expr, name: &str) -> bool {
        // A bare identifier reached in a value position IS the move.
        let bare = |x: &Expr| matches!(&x.kind, ExprKind::Identifier(n) if n == name);
        // Recurse into a borrow position: a bare `name` there is fine, but
        // anything nested deeper gets the full walk.
        let borrow_pos = |s: &Self, x: &Expr| !bare(x) && s.borrow_binding_escapes(x, name);
        match &e.kind {
            ExprKind::Identifier(n) => n == name,
            ExprKind::Integer(..)
            | ExprKind::Float(..)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::ByteStringLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(_)
            | ExprKind::Path { .. }
            | ExprKind::SelfValue
            | ExprKind::SelfType
            | ExprKind::PipePlaceholder
            | ExprKind::OffsetOf { .. }
            | ExprKind::Continue { .. }
            | ExprKind::Error => false,
            ExprKind::InterpolatedStringLit(parts) => parts.iter().any(|p| match p {
                ParsedInterpolationPart::Text(_) => false,
                ParsedInterpolationPart::Expr(x, _) => borrow_pos(self, x),
            }),
            ExprKind::Binary { left, right, .. } => {
                borrow_pos(self, left) || borrow_pos(self, right)
            }
            ExprKind::Unary { operand, .. } => borrow_pos(self, operand),
            ExprKind::Cast { expr, .. } => borrow_pos(self, expr),
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                borrow_pos(self, object)
            }
            ExprKind::Index { object, index } => {
                borrow_pos(self, object) || borrow_pos(self, index)
            }
            ExprKind::MethodCall { object, args, .. } => {
                borrow_pos(self, object)
                    || args
                        .iter()
                        .any(|a| self.borrow_binding_escapes(&a.value, name))
            }
            ExprKind::OptionalChain { object, args, .. } => {
                borrow_pos(self, object)
                    || args
                        .iter()
                        .flatten()
                        .any(|a| self.borrow_binding_escapes(&a.value, name))
            }
            ExprKind::Call { callee, args } => {
                // `println`-family builtins borrow their args.
                let is_print = matches!(
                    &callee.kind,
                    ExprKind::Identifier(f)
                        if matches!(f.as_str(), "println" | "print" | "eprintln" | "eprint")
                );
                // B-2026-08-08-25 leg 1 — a closure LITERAL callee is
                // TRANSPARENT to this walk. Everywhere else "passed to a call"
                // has to mean "moved", because the callee's body is not in hand;
                // for a literal it is, so the question "does this argument
                // escape" is answerable exactly rather than conservatively:
                // it escapes iff the closure's body escapes the matching param.
                //
                // This is what makes `.map` reach the caller-retains classifier.
                // `compile_map_via_match_synthesis` lowers `o.map(f)` to
                // `match o { Some(x) => Some(f(x)) … }` — a bare payload binding
                // handed to a call — so `no_arm_payload_escapes` said "escapes"
                // for every mapper alike and the whole family stayed on the
                // transfer path, leaving `o` dangling for any later read.
                //
                // The distinction it now draws is the real one: `|s|
                // s.to_uppercase()` and `|s| s + "!"` only READ their parameter
                // (the `MethodCall` / `Binary` arms treat a bare operand as a
                // borrow), so they no longer count as escapes, while `|s| s`
                // hands the payload straight out and still does.
                //
                // Only a BARE `name` argument gets this treatment. Anything
                // nested deeper (`f(g(x))`, `f(x.clone())`) keeps the full walk,
                // and an arity or param-shape mismatch answers "escapes" — the
                // safe direction, since a wrong answer here keeps today's
                // transfer rather than inventing a double free.
                let closure_arg_escapes = |s: &Self, i: usize| -> bool {
                    let ExprKind::Closure { params, body, .. } = &callee.kind else {
                        return true;
                    };
                    if params.len() != args.len() {
                        return true;
                    }
                    match params.get(i).map(|p| &p.pattern.kind) {
                        Some(PatternKind::Binding(p)) => s.borrow_binding_escapes(body, p),
                        _ => true,
                    }
                };
                // B-2026-08-30-52 — an ARITHMETIC OR COMPARISON OPERATOR IS A
                // CALL BY THE TIME THIS WALK RUNS. `lowering.rs` rewrites
                // `n + 1` to `Call { callee: Path(["i64", "add"]), .. }`, so
                // the `Binary` arm above — which treats a bare operand as a
                // BORROW — never sees it, and the operand fell to the generic
                // "passed to a call means moved" rule below.
                //
                // The consequence was a read-only arm classified as escaping
                // because of a SCALAR: `match a { Some(S { v, n }) =>
                // println(v[0] + n) }` took the payload and the source read
                // back garbage, while `println(v[0] + 1)` on the identical
                // pattern was correct. Nothing is moved either way — the
                // difference was only whether an operand happened to be a
                // binding.
                //
                // This is the SAME defect B-2026-08-05-3 (guard leg) fixed in
                // the sibling classifier `consume_class`, with the same
                // symptom there: a guard `Some(x) if x.1 == 5i64` scored
                // "consumed" purely because of the desugar, disarming the
                // source and leaking a tuple payload. That fix introduced
                // `is_lowered_primitive_operator`; this walker simply never
                // learned about it. Reusing the one predicate is what keeps
                // the two classifiers from drifting again — an earlier
                // attempt here open-coded a looser test (any method name on a
                // numeric type, and no `String`) and leaked 3200 bytes in
                // `asan_freshtemp_result_arm_binding_a_struct_payload_owns_it_once`.
                let callee_is_primitive_operator =
                    self.pattern_state.escape_walk_relaxes_primitive_operators
                        && super::consume_class::is_lowered_primitive_operator(callee);
                let callee_is_closure_literal = matches!(&callee.kind, ExprKind::Closure { .. });
                // The callee walk still runs for a literal: it is how a closure
                // that CAPTURES `name` (rather than receiving it) is caught.
                self.borrow_binding_escapes(callee, name)
                    || args.iter().enumerate().any(|(i, a)| {
                        if is_print || callee_is_primitive_operator {
                            borrow_pos(self, &a.value)
                        } else if callee_is_closure_literal && bare(&a.value) {
                            closure_arg_escapes(self, i)
                        } else {
                            self.borrow_binding_escapes(&a.value, name)
                        }
                    })
            }
            ExprKind::Question(x) => self.borrow_binding_escapes(x, name),
            ExprKind::NilCoalesce { left, right } | ExprKind::Pipe { left, right } => {
                self.borrow_binding_escapes(left, name) || self.borrow_binding_escapes(right, name)
            }
            ExprKind::Block(b)
            | ExprKind::Comptime(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => self.borrow_binding_escapes_block(b, name),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.borrow_binding_escapes(condition, name)
                    || self.borrow_binding_escapes_block(then_block, name)
                    || else_branch
                        .as_deref()
                        .is_some_and(|x| self.borrow_binding_escapes(x, name))
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                self.borrow_binding_escapes(value, name)
                    || self.borrow_binding_escapes_block(then_block, name)
                    || else_branch
                        .as_deref()
                        .is_some_and(|x| self.borrow_binding_escapes(x, name))
            }
            ExprKind::Match { scrutinee, arms } => {
                // B-2026-08-30-52 (b). A bare-`name` scrutinee USED to be an
                // unconditional move ("could destructure the alias's payload
                // out — conservative move"), which is what kept
                // `match a { Some(p) => match p { A { s } => println(s) } }`
                // off the borrow path: the outer classifier asked whether `p`
                // escapes its arm, and the inner `match` said yes before
                // anything about the inner arms was looked at.
                //
                // The refinement asks that question one level down instead: an
                // inner `match` takes the payload only if some inner arm's OWN
                // bindings escape THAT arm, which is exactly what
                // `no_arm_payload_escapes` already answers — recursively, so
                // the reasoning composes to any nesting depth. Every other
                // scrutinee shape keeps the full walk, and an unrecognized
                // inner body still defaults to "escapes", so the conservative
                // direction is preserved where it was actually doing work.
                let scrutinee_moves = if bare(scrutinee) {
                    !self.no_arm_payload_escapes(arms)
                } else {
                    self.borrow_binding_escapes(scrutinee, name)
                };
                scrutinee_moves
                    || arms.iter().any(|a| {
                        a.guard
                            .as_ref()
                            .is_some_and(|g| self.borrow_binding_escapes(g, name))
                            || self.borrow_binding_escapes(&a.body, name)
                    })
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.borrow_binding_escapes(condition, name)
                    || self.borrow_binding_escapes_block(body, name)
            }
            ExprKind::WhileLet { value, body, .. } => {
                self.borrow_binding_escapes(value, name)
                    || self.borrow_binding_escapes_block(body, name)
            }
            ExprKind::For { iterable, body, .. } => {
                self.borrow_binding_escapes(iterable, name)
                    || self.borrow_binding_escapes_block(body, name)
            }
            ExprKind::Loop { body, .. } | ExprKind::LabeledBlock { body, .. } => {
                self.borrow_binding_escapes_block(body, name)
            }
            // A closure capturing the name (any mode) — conservative move.
            ExprKind::Closure { body, .. } => self.borrow_binding_escapes(body, name),
            ExprKind::Return(v) => v
                .as_deref()
                .is_some_and(|x| self.borrow_binding_escapes(x, name)),
            ExprKind::Break { value, .. } => value
                .as_deref()
                .is_some_and(|x| self.borrow_binding_escapes(x, name)),
            ExprKind::Tuple(xs) | ExprKind::ArrayLiteral(xs) => {
                xs.iter().any(|x| self.borrow_binding_escapes(x, name))
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                items.iter().any(|x| self.borrow_binding_escapes(x, name))
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.borrow_binding_escapes(value, name) || self.borrow_binding_escapes(count, name)
            }
            ExprKind::MapLiteral(pairs) => pairs.iter().any(|(k, v)| {
                self.borrow_binding_escapes(k, name) || self.borrow_binding_escapes(v, name)
            }),
            ExprKind::StructLiteral { fields, spread, .. } => {
                fields
                    .iter()
                    .any(|f| self.borrow_binding_escapes(&f.value, name))
                    || spread
                        .as_deref()
                        .is_some_and(|x| self.borrow_binding_escapes(x, name))
            }
            ExprKind::Range { start, end, .. } => {
                start
                    .as_deref()
                    .is_some_and(|x| self.borrow_binding_escapes(x, name))
                    || end
                        .as_deref()
                        .is_some_and(|x| self.borrow_binding_escapes(x, name))
            }
            ExprKind::Lock { mutex, body, .. } => {
                self.borrow_binding_escapes(mutex, name)
                    || self.borrow_binding_escapes_block(body, name)
            }
            ExprKind::Providers { bindings, body } => {
                bindings
                    .iter()
                    .any(|b| self.borrow_binding_escapes(&b.value, name))
                    || self.borrow_binding_escapes_block(body, name)
            }
        }
    }

    /// Block walker for `borrow_binding_escapes`. A block-local `let`
    /// shadowing `name` would end the binding's reach, but tracking shadow
    /// scopes here buys little — treating post-shadow uses as the outer
    /// binding only ever adds a clone (false positive), never misses a
    /// move.
    pub(super) fn borrow_binding_escapes_block(&self, b: &Block, name: &str) -> bool {
        b.stmts.iter().any(|s| match &s.kind {
            StmtKind::Let { value, .. } => self.borrow_binding_escapes(value, name),
            StmtKind::LetUninit { .. } => false,
            StmtKind::LetElse {
                value, else_block, ..
            } => {
                self.borrow_binding_escapes(value, name)
                    || self.borrow_binding_escapes_block(else_block, name)
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.borrow_binding_escapes_block(body, name)
            }
            StmtKind::Assign { target, value } => {
                // Assigning INTO the alias (`x = …`) drops the old aliased
                // value — conservative move; RHS bare `x` is a plain move.
                self.borrow_binding_escapes(target, name)
                    || self.borrow_binding_escapes(value, name)
            }
            StmtKind::MultiAssign { targets, values } => targets
                .iter()
                .chain(values.iter())
                .any(|x| self.borrow_binding_escapes(x, name)),
            StmtKind::CompoundAssign { target, value, .. } => {
                self.borrow_binding_escapes(target, name)
                    || self.borrow_binding_escapes(value, name)
            }
            StmtKind::Expr(x) => self.borrow_binding_escapes(x, name),
        }) || b
            .final_expr
            .as_deref()
            .is_some_and(|x| self.borrow_binding_escapes(x, name))
    }

    /// Returns an i1 (bool) value: 1 if the scrutinee matches the pattern.
    pub(super) fn compile_pattern_condition(
        &mut self,
        pattern: &Pattern,
        scrut: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let tru = self.context.bool_type().const_int(1, false);
        match &pattern.kind {
            PatternKind::Wildcard => Ok(tru.into()),
            PatternKind::Binding(name) => {
                // Check if this binding name is actually a unit enum variant.
                // The parser produces Binding("Color.Red") or Binding("Red") for
                // unit variants in match arms; detect and compare tags.
                let variant_name = name.rsplit('.').next().unwrap_or(name);
                // Prefer the qualified `Enum.Variant` tag (honors the `IoError`
                // in `IoError.PermissionDenied`) — `enum_tag_for_variant` is
                // bare-name and ambiguous when the variant collides across
                // seeded enums (`PermissionDenied` is in both `IoError` and
                // `TlsError`). Falls back to the bare lookup for unqualified
                // variants. B-2026-06-14 baked-enum companion bug.
                if let Some(tag) = self
                    .variant_pattern_enum_and_tag(pattern)
                    .map(|(_, t)| t)
                    .or_else(|| self.enum_tag_for_variant(variant_name))
                {
                    let actual_tag = self.extract_enum_tag(scrut, variant_name)?;
                    let expected_tag = self.context.i64_type().const_int(tag, false);
                    return Ok(self
                        .builder
                        .build_int_compare(IntPredicate::EQ, actual_tag, expected_tag, "tag_eq")
                        .unwrap()
                        .into());
                }
                // B-2026-08-22-1 — FAIL CLOSED on a QUALIFIED name that
                // resolved to no tag. A Kāra binding is a single identifier,
                // so a `Binding` carrying a dot is never a real binding: it is
                // an `Enum.Variant` pattern (the shape this arm's comment
                // describes) whose enum has no `enum_layouts` entry. Treating
                // it as a catch-all is what made `MemoryOrdering` a SILENT
                // miscompile — every arm always matched, so
                // `match m { MemoryOrdering.SeqCst => … }` ran the first arm
                // and AOT printed `Relaxed` where `--interp` printed `SeqCst`,
                // with no diagnostic anywhere.
                //
                // Seeding that enum fixed the instance; this fixes the CLASS,
                // so the next stdlib enum that reaches codegen without a layout
                // is a build error naming the missing piece instead of a wrong
                // answer. The BARE spelling (`SeqCst`) cannot be guarded here —
                // a bare PascalCase name is indistinguishable from a legitimate
                // binding without the typechecker's resolution — which is
                // exactly why the qualified half is worth catching.
                if name.contains('.') {
                    return Err(format!(
                        "codegen: `{name}` is an enum-variant pattern whose enum has no layout, \
                         so it cannot be compiled to a tag test. The enum reaches the \
                         typechecker through STDLIB_PROGRAMS but not codegen — add it to \
                         `compiled_stdlib_programs` or seed it with `seed_unit_enum` in \
                         `declarations.rs`."
                    ));
                }
                // Not a variant — true binding, always matches.
                Ok(tru.into())
            }
            PatternKind::Literal(lit) => {
                let lit_val = match lit {
                    LiteralPattern::Integer(n, sfx) => self.const_int_for_suffix(*n, *sfx).into(),
                    LiteralPattern::Bool(b) => self
                        .context
                        .bool_type()
                        .const_int(u64::from(*b), false)
                        .into(),
                    LiteralPattern::Float(f, sfx) => self.const_float_for_suffix(*f, *sfx).into(),
                    LiteralPattern::Char(c) => {
                        self.context.i32_type().const_int(*c as u64, false).into()
                    }
                    // Build a full String struct `{ data, len, cap }` for
                    // the literal pattern, matching `ExprKind::StringLit`'s
                    // codegen (`src/codegen/exprs.rs:39-61`). The scrutinee
                    // on this path is always a String struct value (matches
                    // on String typecheck to `String == String`), so both
                    // operands hit `compile_string_binop`'s `BinOp::Eq` arm
                    // — length check + `memcmp` — instead of falling into
                    // the int-path which would panic at
                    // `expr_ops.rs:1138 lhs.into_int_value()`. `cap = 0`
                    // marks the buffer as static (mirrors StringLit so the
                    // pattern doesn't claim ownership of the .rodata bytes).
                    LiteralPattern::String(s) => {
                        let global = self.builder.build_global_string_ptr(s, "spat").unwrap();
                        let str_ty = self.vec_struct_type();
                        let i64_t = self.context.i64_type();
                        let len = i64_t.const_int(s.len() as u64, false);
                        let cap_zero = i64_t.const_int(0, false);
                        let mut agg = str_ty.get_undef();
                        agg = self
                            .builder
                            .build_insert_value(agg, global.as_pointer_value(), 0, "spat.data")
                            .unwrap()
                            .into_struct_value();
                        agg = self
                            .builder
                            .build_insert_value(agg, len, 1, "spat.len")
                            .unwrap()
                            .into_struct_value();
                        agg = self
                            .builder
                            .build_insert_value(agg, cap_zero, 2, "spat.cap")
                            .unwrap()
                            .into_struct_value();
                        agg.into()
                    }
                };
                self.compile_binop(&BinOp::Eq, scrut, lit_val)
            }
            // `lo..=hi` / `lo..hi` / `..=hi` / `lo..` — lower to the
            // bound comparisons `scrut >= lo` and `scrut <(=) hi`, AND'd
            // together. Without this arm the pattern fell through to the
            // catch-all `_ => true` below, so every range matched
            // unconditionally (codegen-only bug; the interpreter was
            // correct). The parser admits only integer / char bounds.
            PatternKind::RangePattern {
                start,
                end,
                inclusive,
            } => {
                // Unsigned comparison only when a bound carries an
                // unsigned int type (e.g. the `b'a'..=b'z'` desugar → U8,
                // or a const named `MAX: u8`). Keeps byte ranges correct
                // for values ≥ 128; signed for plain int / char ranges.
                let unsigned = [start.as_ref(), end.as_ref()]
                    .into_iter()
                    .flatten()
                    .any(|b| self.range_bound_unsigned(b));

                let mut cond: Option<inkwell::values::IntValue<'ctx>> = None;
                if let Some(lo) = start {
                    let lo_val = self.compile_range_bound(lo)?;
                    let ge = self
                        .compile_binop_typed(&BinOp::GtEq, scrut, lo_val, unsigned)?
                        .into_int_value();
                    cond = Some(ge);
                }
                if let Some(hi) = end {
                    let hi_val = self.compile_range_bound(hi)?;
                    let op = if *inclusive { BinOp::LtEq } else { BinOp::Lt };
                    let cmp = self
                        .compile_binop_typed(&op, scrut, hi_val, unsigned)?
                        .into_int_value();
                    cond = Some(match cond {
                        Some(c) => self.builder.build_and(c, cmp, "range.and").unwrap(),
                        None => cmp,
                    });
                }
                // Bare `..` (both None) is rejected by the parser; if it
                // somehow reaches here, treat as always-match.
                Ok(cond.map(|c| c.into()).unwrap_or(tru.into()))
            }
            PatternKind::Or(pats) => {
                let mut result: BasicValueEnum<'ctx> =
                    self.context.bool_type().const_int(0, false).into();
                for p in pats {
                    let cond = self.compile_pattern_condition(p, scrut)?;
                    result = self
                        .builder
                        .build_or(result.into_int_value(), cond.into_int_value(), "orcond")
                        .unwrap()
                        .into();
                }
                Ok(result)
            }
            // Tuple enum variant: check the tag matches, then AND in the
            // tag checks for any nested variant sub-pattern (`E.A(c)` of
            // `Result.Err(E.A(c))`) — see `and_in_nested_variant_conditions`.
            PatternKind::TupleVariant { path, patterns } => {
                let variant_name = path.last().map(|s| s.as_str()).unwrap_or("");
                // Prefer the qualified `Enum.Variant` tag (see the `Binding`
                // arm above) — the bare `enum_tag_for_variant` mis-resolves
                // `IoError.Other` to a colliding seeded `Other`. B-2026-06-14.
                if let Some(tag) = self
                    .variant_pattern_enum_and_tag(pattern)
                    .map(|(_, t)| t)
                    .or_else(|| self.enum_tag_for_variant(variant_name))
                {
                    let actual_tag = self.extract_enum_tag(scrut, variant_name)?;
                    let expected_tag = self.context.i64_type().const_int(tag, false);
                    let cond = self
                        .builder
                        .build_int_compare(IntPredicate::EQ, actual_tag, expected_tag, "tag_eq")
                        .unwrap();
                    let cond =
                        self.and_in_nested_variant_conditions(scrut, variant_name, patterns, cond)?;
                    return Ok(cond.into());
                }
                Ok(tru.into())
            }
            // Struct enum variant: check tag matches (struct-variant
            // nested-variant condition recursion is a follow-up — its
            // field-by-name extraction differs from the positional
            // tuple-variant path; binding still works via
            // `bind_pattern_values`).
            // B-2026-08-04-5 — `!struct_pattern_names_a_user_struct` keeps a
            // bare `S { .. }` destructure of a *plain struct* out of this arm
            // when some enum in scope (a prelude one, typically) happens to
            // declare a variant of the same name. `enum_tag_for_variant` is a
            // bare-name lookup with no struct awareness, so `struct Item` +
            // `enum Holder { Item { .. } }` used to land here, tag-compare the
            // struct's first field against `Holder.Item`'s tag, and fall
            // through every arm. A qualified path (`Holder.Item { .. }`) or a
            // scrutinee that really is the enum still takes this arm — only the
            // ambiguous bare-name-over-a-struct case is redirected.
            PatternKind::Struct { path, .. }
                if !self.struct_pattern_names_a_user_struct(pattern)
                    && (path.len() > 1
                        || self
                            .enum_tag_for_variant(path.last().map(|s| s.as_str()).unwrap_or(""))
                            .is_some()) =>
            {
                let variant_name = path.last().map(|s| s.as_str()).unwrap_or("");
                // Qualified-preferring tag (see the `Binding` / `TupleVariant`
                // arms) so a struct-variant `Enum.V { .. }` resolves against
                // its own layout. B-2026-06-14.
                if let Some(tag) = self
                    .variant_pattern_enum_and_tag(pattern)
                    .map(|(_, t)| t)
                    .or_else(|| self.enum_tag_for_variant(variant_name))
                {
                    let actual_tag = self.extract_enum_tag(scrut, variant_name)?;
                    let expected_tag = self.context.i64_type().const_int(tag, false);
                    return Ok(self
                        .builder
                        .build_int_compare(IntPredicate::EQ, actual_tag, expected_tag, "tag_eq")
                        .unwrap()
                        .into());
                }
                Ok(tru.into())
            }
            // Plain struct pattern `S { f0: p0, f1: p1, .. }` (the enum-variant
            // struct case is handled by the guarded arm above; this unguarded
            // arm is only reached when that guard is false — i.e. `S` is a plain
            // struct, not an enum variant). Extract each field and AND its
            // sub-pattern's condition — recursively, so a nested enum-variant /
            // literal / range field emits its own discriminating test. Without
            // this arm the whole struct pattern fell through to the catch-all
            // `_ => true` below, so EVERY field sub-pattern's test was dropped:
            // `Item { shape: Shape.Circle(r), .. }` matched a `Rect` / `Point`
            // item too, binding garbage — the struct sibling of the tuple
            // "no discriminating test emitted" class (B-2026-07-12-13). A
            // binding / wildcard field recurses to an always-true leaf, so
            // `S { x: 0, y: _ }` correctly tests only `x`.
            PatternKind::Struct { path, fields, .. } => {
                let Some(struct_name) = path.last() else {
                    return Ok(tru.into());
                };
                let Some(field_names) = self
                    .type_decls
                    .struct_field_names
                    .get(struct_name.as_str())
                    .cloned()
                else {
                    // Not a known plain struct (should not happen post-resolve);
                    // match conservatively rather than mis-extract.
                    return Ok(tru.into());
                };
                let struct_ty = self
                    .type_decls
                    .struct_types
                    .get(struct_name.as_str())
                    .copied();
                let mut cond = tru;
                for fp in fields {
                    // Field shorthand (`S { x }`) binds `x` and adds no test.
                    let Some(sub) = &fp.pattern else { continue };
                    let Some(idx) = field_names.iter().position(|n| n == &fp.name) else {
                        continue;
                    };
                    // Pull the field value in whichever form the scrutinee is:
                    // a loaded struct value (owned) → extractvalue; a pointer
                    // (a `ref`/borrowed struct) → GEP + load. Loading yields the
                    // right shape for the recursion (an inline enum field loads
                    // as a StructValue; a shared/boxed enum field as a pointer),
                    // both of which `extract_enum_tag` handles.
                    let field_val: BasicValueEnum<'ctx> = match scrut {
                        BasicValueEnum::StructValue(sv) => self
                            .builder
                            .build_extract_value(sv, idx as u32, "struct.cond.field")
                            .unwrap(),
                        BasicValueEnum::PointerValue(ptr) => {
                            let Some(st) = struct_ty else {
                                continue;
                            };
                            let Ok(field_ptr) = self.builder.build_struct_gep(
                                st,
                                ptr,
                                idx as u32,
                                "struct.cond.fp",
                            ) else {
                                continue;
                            };
                            let Some(field_ty) = st.get_field_type_at_index(idx as u32) else {
                                continue;
                            };
                            self.builder
                                .build_load(field_ty, field_ptr, "struct.cond.load")
                                .unwrap()
                        }
                        _ => continue,
                    };
                    let sub_cond = self
                        .compile_pattern_condition(sub, field_val)?
                        .into_int_value();
                    cond = self
                        .builder
                        .build_and(cond, sub_cond, "struct.and")
                        .unwrap();
                }
                Ok(cond.into())
            }
            // `name @ subpattern` — the alias binding is irrefutable, so
            // the match condition is exactly the sub-pattern's condition
            // (`code @ 500..=599` tests the range; `whole @ Some(x)` tests
            // the variant tag). Without this arm the pattern fell through
            // to the catch-all `_ => true`, so every `@` binding matched
            // unconditionally — the same codegen-only gap the binding side
            // had (see `bind_pattern_values`).
            PatternKind::AtBinding { pattern: inner, .. } => {
                self.compile_pattern_condition(inner, scrut)
            }
            // Plain tuple pattern `(p0, p1, ...)`: the scrutinee is an aggregate
            // struct value. Extract each element and AND the per-element
            // sub-pattern conditions together (recursively — a nested tuple /
            // literal / range element emits its own test). Without this arm the
            // tuple pattern fell through to the catch-all `_ => true` below, so
            // EVERY tuple pattern matched unconditionally and the first tuple
            // arm always won regardless of the element values — the tuple
            // sibling of the range / guard "no discriminating test emitted"
            // class (B-2026-07-12-13). Binding / wildcard elements recurse to
            // an always-true leaf, so `(x, 0)` correctly tests only the second
            // element.
            PatternKind::Tuple(elems) => {
                if let BasicValueEnum::StructValue(sv) = scrut {
                    let mut cond = tru;
                    for (idx, sub) in elems.iter().enumerate() {
                        let elem = self
                            .builder
                            .build_extract_value(sv, idx as u32, "tup.cond.elem")
                            .unwrap();
                        let sub_cond = self.compile_pattern_condition(sub, elem)?.into_int_value();
                        cond = self.builder.build_and(cond, sub_cond, "tup.and").unwrap();
                    }
                    Ok(cond.into())
                } else {
                    // A non-aggregate scrutinee under a tuple pattern would be a
                    // type error upstream; match rather than mis-extract.
                    Ok(tru.into())
                }
            }
            // Plain struct pattern or anything else — always matches
            _ => Ok(tru.into()),
        }
    }

    /// Build an LLVM constant for a range-pattern bound. The parser admits
    /// only integer and char literals in range position; both are built
    /// the same way the `Literal` arm builds them so the comparison is
    /// width-matched to the scrutinee. Float / String / Bool can't appear
    /// here (parser rejects), so they fall back to an i64 0.
    fn range_bound_const(&self, lit: &LiteralPattern) -> BasicValueEnum<'ctx> {
        match lit {
            LiteralPattern::Integer(n, sfx) => self.const_int_for_suffix(*n, *sfx).into(),
            LiteralPattern::Char(c) => self.context.i32_type().const_int(*c as u64, false).into(),
            _ => self.context.i64_type().const_int(0, false).into(),
        }
    }

    /// Compile a range-pattern bound to an `IntValue` for the comparison.
    /// A `Path` bound names a module-level const; reuse the const-identifier
    /// compile path (`consts` map → re-compile the stored initializer, which
    /// LLVM folds), so const-referencing-const initializers work too.
    fn compile_range_bound(&mut self, bound: &RangeBound) -> Result<BasicValueEnum<'ctx>, String> {
        match bound {
            RangeBound::Literal(lit) => Ok(self.range_bound_const(lit)),
            RangeBound::Path { segments, span } => {
                let expr = Expr {
                    kind: if segments.len() == 1 {
                        ExprKind::Identifier(segments[0].clone())
                    } else {
                        ExprKind::Path {
                            segments: segments.clone(),
                            generic_args: None,
                        }
                    },
                    span: *span,
                };
                self.compile_expr(&expr)
            }
        }
    }

    /// Whether a range bound's type is unsigned — drives signed-vs-unsigned
    /// comparison. Literal bounds inspect the int suffix; a const-path bound
    /// resolves the named const's literal initializer type.
    fn range_bound_unsigned(&self, bound: &RangeBound) -> bool {
        use crate::prelude::ConstValue;
        let cv = match bound {
            RangeBound::Literal(lit) => {
                return matches!(
                    lit,
                    LiteralPattern::Integer(
                        _,
                        Some(
                            crate::token::IntSuffix::U8
                                | crate::token::IntSuffix::U16
                                | crate::token::IntSuffix::U32
                                | crate::token::IntSuffix::U64
                                | crate::token::IntSuffix::U128
                                | crate::token::IntSuffix::Usize
                        )
                    )
                );
            }
            RangeBound::Path { segments, .. } if segments.len() == 1 => self
                .mod_bindings
                .consts
                .get(&segments[0])
                .and_then(crate::codegen::helpers::const_value_from_literal_expr),
            RangeBound::Path { .. } => None,
        };
        matches!(
            cv,
            Some(
                ConstValue::U8(_)
                    | ConstValue::U16(_)
                    | ConstValue::U32(_)
                    | ConstValue::U64(_)
                    | ConstValue::U128(_)
                    | ConstValue::Usize(_)
            )
        )
    }

    /// Extract the tag integer from an enum scrutinee.
    /// Handles both shared enums (pointer — GEP to tag at index 1) and
    /// non-shared enums (struct value — extractvalue at index 0).
    pub(super) fn extract_enum_tag(
        &self,
        scrut: BasicValueEnum<'ctx>,
        variant_name: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, String> {
        let i64_t = self.context.i64_type();
        // Check if this variant belongs to a shared enum.
        if let BasicValueEnum::PointerValue(ptr) = scrut {
            for (enum_name, layout) in &self.type_decls.enum_layouts {
                if layout.tags.contains_key(variant_name) {
                    if let Some(info) = self.type_decls.shared_types.get(enum_name) {
                        // Shared enum: tag is at heap index 1.
                        let tag_ptr = self
                            .builder
                            .build_struct_gep(info.heap_type, ptr, 1, "sh_tag_ptr")
                            .unwrap();
                        let tag = self
                            .builder
                            .build_load(i64_t, tag_ptr, "actual_tag")
                            .unwrap()
                            .into_int_value();
                        return Ok(tag);
                    }
                }
            }
        }
        // Non-shared enum: extractvalue at index 0.
        if let BasicValueEnum::StructValue(sv) = scrut {
            let tag = self
                .builder
                .build_extract_value(sv, 0, "actual_tag")
                .unwrap()
                .into_int_value();
            return Ok(tag);
        }
        Ok(i64_t.const_int(0, false))
    }

    /// Pick the enum that owns a bare `variant_name`, **without consulting
    /// HashMap iteration order**, applying `accept` as an extra filter.
    ///
    /// Preference, in order:
    ///   1. the match scrutinee's own enum, when it declares the variant —
    ///      the `#39` rule the resolvers above already apply;
    ///   2. the lexicographically-first user-declared enum;
    ///   3. the lexicographically-first seeded built-in (`Option`, `Result`,
    ///      `Json`, `TcpError`, …).
    ///
    /// B-2026-08-20-26: steps 2 and 3 used to be `get_or_insert` over an
    /// `enum_layouts` iteration, i.e. "whichever the unordered map yields
    /// first". When two USER enums share a variant name — which the scrutinee
    /// hint normally resolves, but not on the paths that have no hint to
    /// consult — that made a codegen decision a function of the per-process
    /// HashMap seed. Ordering by name does not make an ambiguous case
    /// *correct*, but it makes it REPRODUCIBLE, so a wrong pick fails the same
    /// way on every run instead of hiding behind a seed.
    pub(super) fn owning_enum_for_variant(
        &self,
        variant_name: &str,
        mut accept: impl FnMut(&str, &super::state::EnumLayout<'ctx>) -> bool,
    ) -> Option<&str> {
        if let Some(hint) = self.pattern_state.match_scrutinee_enum_hint.as_deref() {
            if let Some(l) = self.type_decls.enum_layouts.get(hint) {
                if l.tags.contains_key(variant_name) && accept(hint, l) {
                    return Some(hint);
                }
            }
        }
        let mut user_hit: Option<&str> = None;
        let mut seed_hit: Option<&str> = None;
        for (en, l) in &self.type_decls.enum_layouts {
            if !l.tags.contains_key(variant_name) || !accept(en, l) {
                continue;
            }
            let slot = if self.type_decls.seeded_enum_names.contains(en) {
                &mut seed_hit
            } else {
                &mut user_hit
            };
            if slot.is_none_or(|seen| en.as_str() < seen) {
                *slot = Some(en.as_str());
            }
        }
        user_hit.or(seed_hit)
    }

    /// Find the discriminant tag for a variant name across all registered enums.
    /// Prefers user-declared enums over the seeded built-ins (`Option`,
    /// `Result`, `Json`, `TcpError`, …) when the variant name collides
    /// (e.g. user `MyIoErr.Other` vs seeded `TcpError.Other`). Without
    /// the preference, HashMap iteration order picks one at random — the
    /// 2026-05-25 codegen-suite intermittent-hang investigation found
    /// that the match-dispatch site sometimes loaded the wrong enum's
    /// tag, sending all comparisons down the wildcard `_` arm.
    pub(super) fn enum_tag_for_variant(&self, variant_name: &str) -> Option<u64> {
        let en = self.owning_enum_for_variant(variant_name, |_, _| true)?;
        self.type_decls
            .enum_layouts
            .get(en)
            .and_then(|l| l.tags.get(variant_name))
            .copied()
    }

    /// Resolve the tagged-union LLVM struct type for a *variant* sub-
    /// pattern (`E.A(c)` / `E.S { .. }`), or `None` if the pattern is not
    /// an enum-variant pattern. Prefers the qualified enum segment in the
    /// path (`E` in `E.A`) so a variant-name collision across enums
    /// resolves deterministically; falls back to the user-vs-seed-preferred
    /// variant-name lookup. Used by `reconstruct_payload_value` (and the
    /// payload word-count / llvm-type helpers) to rebuild a nested inner
    /// enum value from its payload words. A plain (non-enum) struct pattern
    /// returns `None` here — its last path segment isn't a known variant —
    /// so it falls through to the struct-reconstruction path.
    pub(super) fn enum_layout_type_for_variant_pattern(
        &self,
        pat: &Pattern,
    ) -> Option<StructType<'ctx>> {
        self.variant_pattern_enum_and_tag(pat).map(|(ty, _)| ty)
    }

    /// B-2026-08-04-5 — is this a bare `Name { .. }` destructure whose
    /// single-segment path names a *user struct*? Then it is unambiguously a
    /// struct pattern: no Kāra syntax spells a unit variant that way, so the
    /// unqualified variant-name scan below must decline rather than hand back
    /// whichever enum happens to carry a variant of the same name.
    ///
    /// The prelude alone seeds 14 enums whose variant names include `Full`,
    /// `Display`, `Object`, `Number`, `Int`, `Timeout`, `NotFound`, `Other`
    /// and `Env` — all plausible user struct names. A user `struct Full`
    /// destructured as `Some(Full { name, buf })` resolved to
    /// `ChannelError`, whose unit-only layout is `{ i64 }`; the payload
    /// sizing arms then fell to their 1-word / i64 defaults, the debox never
    /// fired, and the struct bind arm's `build_extract_value(sv, 1)` on a
    /// 1-field aggregate ICE'd with `ExtractOutOfRange`.
    ///
    /// A QUALIFIED path (`Holder.Item { .. }`) and a scrutinee whose enum
    /// really declares the variant both keep their enum reading — the first
    /// via the `path.len() == 1` test, the second via the scrutinee-hint
    /// escape below — so only the genuinely ambiguous bare-name-over-a-struct
    /// case is redirected. Restricted to `PatternKind::Struct` on purpose: a
    /// bare `Int(v)` tuple-variant pattern must still resolve through the
    /// scrutinee hint even when a `struct Int` is in scope.
    /// Does this pattern BIND at least one name, as opposed to only testing?
    /// Drives ownership questions where "who frees this" hinges on whether a
    /// leaf binding took the value: a `_`, a literal, or a range binds
    /// nothing, so whatever held the value still owns it. Structural patterns
    /// recurse; a struct field written in shorthand (`S { x }`) binds by the
    /// field's own name and needs no sub-pattern.
    ///
    /// Mirrors `use_classifier::pattern_binds_anything` minus its
    /// unit-variant-name check, which needs the resolver's table — codegen's
    /// callers here are already inside a payload destructure, where a bare
    /// name is a binding.
    fn pattern_binds_anything(pat: &Pattern) -> bool {
        match &pat.kind {
            PatternKind::Wildcard | PatternKind::Literal(_) | PatternKind::RangePattern { .. } => {
                false
            }
            PatternKind::Binding(_) => true,
            PatternKind::AtBinding { by_ref, .. } => !by_ref,
            PatternKind::Tuple(pats) | PatternKind::TupleVariant { patterns: pats, .. } => {
                pats.iter().any(Self::pattern_binds_anything)
            }
            PatternKind::Struct { fields, .. } => fields.iter().any(|f| match &f.pattern {
                Some(sub) => Self::pattern_binds_anything(sub),
                None => true,
            }),
            PatternKind::Or(alts) => alts.iter().any(Self::pattern_binds_anything),
            _ => true,
        }
    }

    pub(super) fn struct_pattern_names_a_user_struct(&self, pat: &Pattern) -> bool {
        let PatternKind::Struct { path, .. } = &pat.kind else {
            return false;
        };
        if path.len() != 1 || !self.type_decls.struct_types.contains_key(path[0].as_str()) {
            return false;
        }
        // The match really is over an enum that declares this variant
        // (`match h { Item { a } => .. }` with `h: Holder` and
        // `enum Holder { Item { a: i64 } }`): the pattern is the variant, not
        // the same-named struct. Mirrors the `#39` scrutinee-hint preference
        // the two resolvers apply just above their unqualified scans.
        let hint_claims_it = self
            .pattern_state
            .match_scrutinee_enum_hint
            .as_deref()
            .and_then(|h| self.type_decls.enum_layouts.get(h))
            .is_some_and(|l| l.tags.contains_key(path[0].as_str()));
        !hint_claims_it
    }

    /// Resolve the **enum name** a variant sub-pattern belongs to, by the
    /// same qualified-segment-preferred / user-vs-seed-fallback logic as
    /// [`Self::variant_pattern_enum_and_tag`] (which returns the LLVM type +
    /// tag from one layout). Used by the B-track fresh-temp scrutinee
    /// materialization (`materialize_freshtemp_enum_scrutinee`) — which needs
    /// the *name* to drive `track_enum_var` / `emit_enum_drop_switch` /
    /// `suppress_destructured_enum_payload_cleanup_at`, all keyed on the
    /// `enum_layouts` map by name. `None` for non-variant patterns.
    pub(super) fn variant_pattern_enum_name(&self, pat: &Pattern) -> Option<String> {
        let segments: Vec<&str> = match &pat.kind {
            PatternKind::TupleVariant { path, .. } | PatternKind::Struct { path, .. } => {
                path.iter().map(|s| s.as_str()).collect()
            }
            PatternKind::Binding(name) => name.split('.').collect(),
            _ => return None,
        };
        let variant_name = *segments.last()?;
        if segments.len() >= 2 {
            let qualifier = segments[segments.len() - 2];
            if let Some(layout) = self.type_decls.enum_layouts.get(qualifier) {
                if layout.tags.contains_key(variant_name) {
                    return Some(qualifier.to_string());
                }
            }
        }
        // #39 — an unqualified variant resolves against the match scrutinee's
        // enum first, so a name shared with another imported enum (`Float` in
        // both `Token` and `Expr`) binds to the scrutinee's variant, not
        // whichever the unordered map yields first.
        if let Some(hint) = &self.pattern_state.match_scrutinee_enum_hint {
            if let Some(layout) = self.type_decls.enum_layouts.get(hint) {
                if layout.tags.contains_key(variant_name) {
                    return Some(hint.clone());
                }
            }
        }
        if self.struct_pattern_names_a_user_struct(pat) {
            return None;
        }
        self.owning_enum_for_variant(variant_name, |_, _| true)
            .map(str::to_string)
    }

    /// Resolve `(enum llvm type, expected tag)` for a *variant* sub-pattern
    /// (`E.A(c)` / `E.S { .. }` / a fieldless `Binding` variant `E.B`), or
    /// `None` if the pattern is not an enum-variant pattern. **The tag and
    /// type come from the SAME layout**, resolved by preferring the
    /// qualified enum segment in the path (`E` in `E.A`). This is load-
    /// bearing for correctness, not just determinism: `TcpError` and
    /// `TlsError` share both the `{i64, i64}` LLVM shape *and* the variant
    /// names `AddrInUse` / `ConnectionRefused` / `PermissionDenied`, so a
    /// bare-name tag lookup (`enum_tag_for_variant`) is genuinely ambiguous
    /// — it can return `TlsError`'s tag for a `TcpError` value and make the
    /// wrong arm match. The qualified path (`TcpError.AddrInUse`) pins the
    /// enum; the unqualified fallback keeps type and tag from one layout so
    /// they at least agree. Used by the nested-variant condition recursion
    /// and `reconstruct_payload_value`.
    pub(super) fn variant_pattern_enum_and_tag(
        &self,
        pat: &Pattern,
    ) -> Option<(StructType<'ctx>, u64)> {
        let segments: Vec<&str> = match &pat.kind {
            PatternKind::TupleVariant { path, .. } | PatternKind::Struct { path, .. } => {
                path.iter().map(|s| s.as_str()).collect()
            }
            PatternKind::Binding(name) => name.split('.').collect(),
            _ => return None,
        };
        let variant_name = *segments.last()?;
        // Qualified `Enum.Variant`: take both type and tag from that enum.
        if segments.len() >= 2 {
            if let Some(layout) = self
                .type_decls
                .enum_layouts
                .get(segments[segments.len() - 2])
            {
                if let Some(&tag) = layout.tags.get(variant_name) {
                    return Some((layout.llvm_type, tag));
                }
            }
        }
        // #39 — prefer the match scrutinee's enum for an unqualified variant,
        // so a name shared across enums (`Token.Float` vs `Expr.Float`) resolves
        // to the scrutinee's tag instead of whichever the unordered map yields.
        if let Some(hint) = &self.pattern_state.match_scrutinee_enum_hint {
            if let Some(layout) = self.type_decls.enum_layouts.get(hint) {
                if let Some(&tag) = layout.tags.get(variant_name) {
                    return Some((layout.llvm_type, tag));
                }
            }
        }
        if self.struct_pattern_names_a_user_struct(pat) {
            return None;
        }
        // Unqualified fallback: user-vs-seed preference, name-ordered so the
        // pick does not ride the map's seed, type + tag from the SAME layout
        // (so a downstream tag compare stays self-consistent).
        let en = self.owning_enum_for_variant(variant_name, |_, _| true)?;
        let l = self.type_decls.enum_layouts.get(en)?;
        Some((l.llvm_type, *l.tags.get(variant_name)?))
    }

    /// Resolve the per-field `(start_word, num_words)` payload offsets for
    /// `variant_name`, preferring the layout whose LLVM type matches the
    /// scrutinee (disambiguates a variant name shared across enums), then
    /// user-declared over seeded enums, falling back to "one word per
    /// field at sequential offsets". Mirrors the inline resolution in
    /// `bind_pattern_values`'s `TupleVariant` arm; shared by the
    /// nested-variant condition recursion.
    fn resolve_variant_field_offsets(
        &self,
        variant_name: &str,
        scrut_struct_ty: Option<StructType<'ctx>>,
        num_patterns: usize,
    ) -> Vec<(usize, usize)> {
        // Both passes go through the scrutinee-hint-preferred, name-ordered
        // resolver. The first narrows by the scrutinee's LLVM struct type;
        // that is NOT a unique key (two enums with identical layouts share one
        // LLVM struct type), so it can still be ambiguous, and it used to be
        // settled by `.find()` over the unordered map — the same seed-decided
        // pick this function's twin in `bind_pattern_values` was taught to
        // avoid in B-2026-08-05-16, and which its extraction here did not
        // inherit (B-2026-08-20-26).
        self.owning_enum_for_variant(variant_name, |_, l| {
            scrut_struct_ty.as_ref().is_none_or(|t| &l.llvm_type == t)
        })
        .or_else(|| self.owning_enum_for_variant(variant_name, |_, _| true))
        .and_then(|en| self.type_decls.enum_layouts.get(en))
        .and_then(|l| l.field_word_offsets.get(variant_name).cloned())
        .unwrap_or_else(|| (0..num_patterns).map(|i| (i, 1)).collect())
    }

    /// Does this payload sub-pattern impose a VALUE test, beyond whatever
    /// tag check its own shape implies? A `Binding` / `Wildcard` leaf only
    /// names the payload, so it always matches and needs no test; a literal
    /// or a range constrains the payload's value and MUST emit one.
    ///
    /// This is the gate that decides whether
    /// `and_in_nested_variant_conditions` reconstructs a payload at all.
    /// Before B-2026-08-20-11 that function keyed exclusively off
    /// `variant_pattern_enum_and_tag`, so a non-variant sub-pattern was
    /// invisible to it and `Some(7)` compiled to the OUTER TAG CHECK ALONE —
    /// every `Some(_)` took the arm, silently, in the compiled binary only
    /// (the interpreter compares properly). Same defect shape as the
    /// range-pattern and struct-field misses already fixed in
    /// `compile_pattern_condition` (B-2026-07-12-13): a sub-pattern that
    /// discriminates, dropped on the floor, so the arm over-matches.
    ///
    /// Compound arms recurse because the test can be nested arbitrarily
    /// deep — `Some((1, x))`, `Some(P { n: 3 })`, `Some(1 | 2)`,
    /// `Some(v @ 5)` all carry one. Variant sub-patterns deliberately return
    /// `false` here: their tag check is the caller's existing job, and
    /// answering `true` would double-count them.
    fn pattern_tests_payload_value(pat: &Pattern) -> bool {
        match &pat.kind {
            PatternKind::Literal(_) | PatternKind::RangePattern { .. } => true,
            PatternKind::Or(ps) | PatternKind::Tuple(ps) => {
                ps.iter().any(Self::pattern_tests_payload_value)
            }
            // A shorthand field (`S { x }`) carries no sub-pattern — it is a
            // plain binding, so it tests nothing.
            PatternKind::Struct { fields, .. } => fields.iter().any(|f| {
                f.pattern
                    .as_ref()
                    .is_some_and(Self::pattern_tests_payload_value)
            }),
            PatternKind::AtBinding { pattern, .. } => Self::pattern_tests_payload_value(pattern),
            _ => false,
        }
    }

    /// AND into `cond` the discriminating tests carried by an outer
    /// variant's payload sub-patterns. Two kinds, both reconstructed from
    /// the same payload words:
    ///
    /// 1. a nested *variant* sub-pattern — the inner `E.A` of
    ///    `Result.Err(E.A(c))` — contributes `inner.tag == expected`;
    /// 2. a sub-pattern that tests the payload's VALUE — `Some(7)`,
    ///    `Some(1..=5)`, `Some((1, x))` — is handed back to
    ///    `compile_pattern_condition` (see `pattern_tests_payload_value`).
    ///
    /// Kind 2 was absent until B-2026-08-20-11: only the outer tag was
    /// tested, so `Some(<literal>)` fired for every `Some(_)` in a compiled
    /// binary while the interpreter got it right — a silent run-vs-build
    /// miscompile.
    /// The outer-tag-only condition matches every `Result.Err(...)`
    /// regardless of the inner variant, so without this a
    /// `Result.Err(E.B)` value would wrongly take a `Result.Err(E.A(c))`
    /// arm (and bind `c` to garbage). For each variant sub-pattern this
    /// extracts its payload words from the (non-shared, struct-value)
    /// scrutinee, rebuilds the inner enum value, and AND-s in
    /// `inner.tag == expected`. The expected tag comes from
    /// `variant_pattern_enum_and_tag` — the qualified-path-disambiguated
    /// layout — NOT the bare-name `enum_tag_for_variant` (which is
    /// ambiguous for `TcpError` / `TlsError`, identical shape + shared
    /// variant names, and would mis-tag). Deeper nesting (a variant inside
    /// this variant's payload) recurses against the rebuilt inner value.
    /// Non-variant sub-patterns (bindings / wildcards / leaves) pass
    /// through unchanged; shared (pointer) enum scrutinees pass through
    /// (their nested-variant condition is a follow-up — the binding side
    /// already handles them). (phase-7-codegen.md — nested enum-payload bind.)
    fn and_in_nested_variant_conditions(
        &mut self,
        scrut: BasicValueEnum<'ctx>,
        outer_variant_name: &str,
        sub_patterns: &[Pattern],
        cond: inkwell::values::IntValue<'ctx>,
    ) -> Result<inkwell::values::IntValue<'ctx>, String> {
        let BasicValueEnum::StructValue(sv) = scrut else {
            return Ok(cond);
        };
        if !sub_patterns.iter().any(|p| {
            self.variant_pattern_enum_and_tag(p).is_some() || Self::pattern_tests_payload_value(p)
        }) {
            return Ok(cond);
        }
        // Lazy gate (B-2026-07-15-5): the nested reconstruction may DEBOX
        // the payload — an oversized nested enum payload is heap-boxed, so
        // `reconstruct_payload_value` emits `inttoptr` + `load` on payload
        // word 0. On a NON-matching outer variant that word is not a box
        // pointer (a `None` payload area is zeros → NULL deref). Emit the
        // inner checks in a block reached only when `cond` (which includes
        // the outer tag check) already passed, and phi the result; the old
        // eager `and` executed the load unconditionally.
        let fn_val = self.current_fn.unwrap();
        let entry_bb = self.builder.get_insert_block().unwrap();
        let check_bb = self.context.append_basic_block(fn_val, "ncond.check");
        let merge_bb = self.context.append_basic_block(fn_val, "ncond.merge");
        self.builder
            .build_conditional_branch(cond, check_bb, merge_bb)
            .unwrap();
        self.builder.position_at_end(check_bb);
        let mut inner_cond = self.context.bool_type().const_int(1, false);
        let offsets = self.resolve_variant_field_offsets(
            outer_variant_name,
            Some(sv.get_type()),
            sub_patterns.len(),
        );
        for (i, sub) in sub_patterns.iter().enumerate() {
            let variant_tag = self.variant_pattern_enum_and_tag(sub).map(|(_, t)| t);
            // Skip leaves that always match (`Some(x)`, `Some(_)`); reconstruct
            // for a nested VARIANT (tag check, below) or for any sub-pattern
            // that tests the payload's VALUE (B-2026-08-20-11).
            if variant_tag.is_none() && !Self::pattern_tests_payload_value(sub) {
                continue;
            }
            let (start_word, num_words) = offsets.get(i).copied().unwrap_or((i, 1));
            let mut field_words: Vec<inkwell::values::IntValue<'ctx>> =
                Vec::with_capacity(num_words);
            for j in 0..num_words {
                let w = self
                    .builder
                    .build_extract_value(sv, (start_word + j + 1) as u32, "ncond.w")
                    .unwrap()
                    .into_int_value();
                field_words.push(w);
            }
            let inner = self.reconstruct_payload_value(sub, &field_words)?;
            // Non-variant sub-pattern: the payload carries no tag of its own,
            // so hand the rebuilt VALUE back to the general pattern-condition
            // compiler. That reuses the same literal / range / tuple / struct /
            // or / at-binding arms the top-level scrutinee gets, which is what
            // makes `Some(7)`, `Some(1..=5)` and `Some("ok")` discriminate
            // identically to their bare `7` / `1..=5` / `"ok"` forms — and it
            // recurses, so an arbitrarily nested test still lands.
            // B-2026-08-20-11.
            let Some(expected_tag) = variant_tag else {
                let value_cond = self.compile_pattern_condition(sub, inner)?;
                inner_cond = self
                    .builder
                    .build_and(inner_cond, value_cond.into_int_value(), "ncond.valand")
                    .unwrap();
                continue;
            };
            // The rebuilt inner value is the enum's `{ tag, payload... }`
            // struct; its tag is field 0. Compare against the
            // qualified-path-resolved expected tag.
            let BasicValueEnum::StructValue(inner_sv) = inner else {
                continue;
            };
            let actual_tag = self
                .builder
                .build_extract_value(inner_sv, 0, "ncond.tag")
                .unwrap()
                .into_int_value();
            let expected = self.context.i64_type().const_int(expected_tag, false);
            let tag_eq = self
                .builder
                .build_int_compare(IntPredicate::EQ, actual_tag, expected, "ncond.tageq")
                .unwrap();
            inner_cond = self
                .builder
                .build_and(inner_cond, tag_eq, "ncond.and")
                .unwrap();
            // Deeper nesting: if this variant's own payload contains further
            // variant sub-patterns, recurse against the rebuilt inner value.
            // The recursion re-applies the same lazy gate on `inner_cond`, so
            // each level's debox load only runs once every enclosing tag has
            // matched.
            if let PatternKind::TupleVariant { path, patterns } = &sub.kind {
                let inner_variant = path.last().map(|s| s.as_str()).unwrap_or("");
                inner_cond = self.and_in_nested_variant_conditions(
                    inner,
                    inner_variant,
                    patterns,
                    inner_cond,
                )?;
            }
        }
        // The recursion above may have moved the insert point into its own
        // merge block — the phi's incoming edge must be the block that
        // actually branches to `merge_bb`.
        let check_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();
        self.builder.position_at_end(merge_bb);
        let fls = self.context.bool_type().const_int(0, false);
        let phi = self
            .builder
            .build_phi(self.context.bool_type(), "ncond.phi")
            .unwrap();
        phi.add_incoming(&[(&fls, entry_bb), (&inner_cond, check_end_bb)]);
        Ok(phi.as_basic_value().into_int_value())
    }

    /// Compound-payload enum codegen (tuple-destructure helper) —
    /// per-element word count for a destructure sub-pattern. Mirrors
    /// the construction-side `payload_word_count_for_type_expr` shape
    /// but reads typechecker-recorded surface names (`pattern_binding_types`)
    /// off the pattern instead of source-level `TypeExpr`. Used by the
    /// Tuple arm in `reconstruct_payload_value` to slice the variant's
    /// flat payload-word vector into per-element ranges.
    ///
    /// - Vec / String → 3 words (vec struct shape)
    /// - Slice → 2 words (slice struct shape)
    /// - Registered user struct → its LLVM word count
    /// - Tuple sub-pattern → recursive sum
    /// - Primitive binding / wildcard / unknown → 1 word
    pub(super) fn pattern_payload_word_count(&self, pat: &Pattern) -> usize {
        match &pat.kind {
            PatternKind::Tuple(elems) => elems
                .iter()
                .map(|p| self.pattern_payload_word_count(p))
                .sum(),
            // A STRING literal sub-pattern (`Some("ok")`) spans the same 3
            // words as a String binding — the `_ => 1` default below would
            // hand `reconstruct_payload_value` a single word and rebuild a
            // bare i64 where `compile_pattern_condition`'s String arm expects
            // a `{ ptr, len, cap }` struct to memcmp against. Every other
            // literal (int / bool / char / float) really is one word.
            // B-2026-08-20-11.
            PatternKind::Literal(LiteralPattern::String(_)) => 3,
            // Nested enum-variant sub-pattern (`Option.Some(x)` as the
            // payload of another variant — `Option.Some(Option.Some(x))`,
            // `Wrap.W(Option.Some(x))`): the payload's natural width is the
            // inner enum's full tagged-union word count, exactly like the
            // enum-typed Binding arm below. Shared enums are RC pointers
            // (1 word — see the shared caveat in `pattern_payload_llvm_type`).
            // Before this arm the `_ => 1` default kept the debox predicate
            // (`want > field_words.len()` in `reconstruct_payload_value`)
            // from ever firing for a heap-BOXED nested enum payload —
            // Option-in-Option (inner 4 words > Option's 3-word area) or any
            // enum payload sized through `payload_word_count_for_type_expr`'s
            // enum-in-enum 1-word carve-out — so the nested tag check
            // compared the box POINTER against the tag and the wrong arm
            // matched silently, and the bind side read undef words
            // (B-2026-07-15-5).
            PatternKind::TupleVariant { .. } => {
                if let Some(name) = self.variant_pattern_enum_name(pat) {
                    if self.type_decls.shared_types.contains_key(&name) {
                        return 1;
                    }
                    if let Some(layout) = self.type_decls.enum_layouts.get(&name) {
                        return Self::llvm_type_word_count(layout.llvm_type.into());
                    }
                }
                1
            }
            PatternKind::Binding(name) => {
                let key = (pat.span.offset, pat.span.length);
                // Unit-variant sub-pattern (`Option.None` inside
                // `Option.Some(Option.None)`): same enum-width rule as the
                // TupleVariant arm above, so the condition-path
                // reconstruction deboxes and tests the real inner tag.
                // Guarded on the name resolving to a variant AND no
                // typechecker-recorded binding type, so ordinary bindings
                // never take this path (B-2026-07-15-5).
                if !self.pattern_state.pattern_binding_types.contains_key(&key) {
                    let variant_name = name.rsplit('.').next().unwrap_or(name);
                    if name.contains('.') || self.enum_tag_for_variant(variant_name).is_some() {
                        if let Some(en) = self.variant_pattern_enum_name(pat) {
                            if self.type_decls.shared_types.contains_key(&en) {
                                return 1;
                            }
                            if let Some(layout) = self.type_decls.enum_layouts.get(&en) {
                                return Self::llvm_type_word_count(layout.llvm_type.into());
                            }
                        }
                    }
                }
                // B-2026-07-13-3: a generic enum's bare-`T` payload binding has
                // no typechecker-recorded surface type, but the active monomorph
                // substitution resolved it to a concrete heap type (String/Vec)
                // stashed in `mono_payload_binding_type_exprs`. Size from THAT so
                // the reconstruct triggers the debox unpack (`want >
                // field_words.len()`) and the arm value lowers at its true width
                // instead of the erased 1-word default. Only consulted when the
                // typechecker recorded nothing (a concrete recorded type wins).
                if let Some(te) = self.mono_payload_binding_type_expr_for(&key) {
                    return Self::llvm_type_word_count(self.llvm_type_for_type_expr(&te)).max(1);
                }
                // Tuple-typed bindings (e.g. `Some(node)` where node is
                // `(i64, i64)`) — sum element widths from the recorded
                // tuple `TypeExpr` so multi-word payloads reconstitute
                // as the right-shaped tuple struct.
                if matches!(
                    self.pattern_state
                        .pattern_binding_types
                        .get(&key)
                        .map(|s| s.as_str()),
                    Some("Tuple")
                ) {
                    if let Some(te) = self.pattern_state.pattern_binding_inner_types.get(&key) {
                        if let TypeKind::Tuple(elems) = &te.kind {
                            return elems
                                .iter()
                                .map(|el| {
                                    Self::llvm_type_word_count(self.llvm_type_for_type_expr(el))
                                })
                                .sum::<usize>()
                                .max(1);
                        }
                    }
                }
                // A concretely-instantiated GENERIC user-struct payload binding
                // (`Err(e)` where `e: Wrap[String]`): size from the MONO
                // instantiation width, not the all-`i64` generic base — the
                // `Some(name)` arm below would look up `struct_types["Wrap"]`
                // (the `{i64}` base = 1 word) and truncate a 3-word `String`
                // field (B-2026-07-12-2 recovery). The inner `TypeExpr` is
                // recorded by the typechecker only for owned/borrow generic
                // struct payload bindings, so this branch never fires for a
                // String/Vec/scalar binding (handled by the explicit arms).
                if let Some(te) = self.pattern_state.pattern_binding_inner_types.get(&key) {
                    if self.is_generic_named_struct_type_expr(te) {
                        return Self::llvm_type_word_count(self.llvm_type_for_type_expr(te)).max(1);
                    }
                }
                match self
                    .pattern_state
                    .pattern_binding_types
                    .get(&key)
                    .map(|s| s.as_str())
                {
                    // VecDeque rides Vec's 3-word `{ptr, len, cap}` layout
                    // (B-2026-06-10-3): without it, a VecDeque enum payload
                    // got the 1-word default → malformed value, crash on use.
                    // `StringSlice` rides the same 3-word layout (a borrowed
                    // view, cap=0) — without it a match-bound StringSlice
                    // payload (`Ok(s)` from `CStr.to_string_slice()`, or a
                    // `String.slice()` result carried through an enum) got the
                    // 1-word default and truncated to just the pointer.
                    // `CString` (design.md § C-String Literals) shares the same
                    // 3-word `{ptr, len, cap}` owning layout as `String`, so an
                    // `Ok(cs)` binding from `Result[CString, NulError]` reconstructs
                    // full-width (without this it took the 1-word default and
                    // truncated to the pointer).
                    Some("Vec") | Some("VecDeque") | Some("String") | Some("StringSlice")
                    | Some("CString") => 3,
                    Some("Slice") => 2,
                    // `Array[T, N]` / `Vector[T, N]` — N element words, from
                    // the concrete `TypeExpr` the typechecker records alongside
                    // the name. Neither resolves through `struct_types` or
                    // `enum_layouts`, so the `Some(name)` arm below returned the
                    // 1-word default: the reconstruction bound word 0 (the
                    // first element, or the box pointer for a payload wider
                    // than the variant's area) instead of the whole value
                    // (B-2026-08-31-18).
                    Some("Array") | Some("Vector") => self
                        .pattern_state
                        .pattern_binding_inner_types
                        .get(&key)
                        .map(|te| Self::llvm_type_word_count(self.llvm_type_for_type_expr(te)))
                        .unwrap_or(1)
                        .max(1),
                    // Shared type (struct OR enum): RC heap pointer = exactly one
                    // word. Must precede the struct/enum arms (see the twin note
                    // in `pattern_payload_llvm_type`) — a direct recursive shared
                    // enum field is one pointer word, not the inline tagged-union
                    // size.
                    Some(name) if self.type_decls.shared_types.contains_key(name) => 1,
                    Some(name) => {
                        self.type_decls
                            .struct_types
                            .get(name)
                            .map(|st| Self::llvm_type_word_count((*st).into()))
                            .or_else(|| {
                                // Enum-typed binding (e.g. `Ok(j)` where j: Json) —
                                // the binding's natural width is the enum's full
                                // tagged-union LLVM struct size. Without this arm
                                // the Some-name branch falls to the i64 default,
                                // which truncates 4-i64 `Json` payloads to a single
                                // word and breaks `match Json.parse(s) { Ok(j) =>
                                // j.stringify() ... }` and any other Result-wrapped
                                // multi-word enum value.
                                self.type_decls.enum_layouts.get(name).map(|layout| {
                                    Self::llvm_type_word_count(layout.llvm_type.into())
                                })
                            })
                            .unwrap_or(1)
                    }
                    None => 1,
                }
            }
            // Struct-pattern payload destructure (slice 3t): `Some(Holder {
            // name, id })` — the payload's natural width is the struct's
            // full LLVM word count. An enum struct-VARIANT pattern
            // (`E.V { .. }`) resolves through its enum layout instead.
            // Before this arm, the `_ => 1` default sized the payload as a
            // single word, the reconstruction bound the raw word, and the
            // struct bind arm's StructValue guard silently missed — every
            // field stayed unbound ("Undefined variable").
            PatternKind::Struct { path, .. } => {
                if let Some(enum_name) = self.variant_pattern_enum_name(pat) {
                    return self
                        .type_decls
                        .enum_layouts
                        .get(&enum_name)
                        .map(|l| Self::llvm_type_word_count(l.llvm_type.into()))
                        .unwrap_or(1);
                }
                path.last()
                    .and_then(|n| self.type_decls.struct_types.get(n.as_str()))
                    .map(|st| Self::llvm_type_word_count((*st).into()))
                    .unwrap_or(1)
            }
            _ => 1,
        }
    }

    /// Compound-payload enum codegen (tuple-destructure helper) —
    /// LLVM type for a destructure sub-pattern's reconstructed value.
    /// Used by the Tuple arm in `reconstruct_payload_value` to build
    /// the surrounding tuple struct type whose fields hold each
    /// element's reconstructed aggregate.
    pub(super) fn pattern_payload_llvm_type(&self, pat: &Pattern) -> BasicTypeEnum<'ctx> {
        match &pat.kind {
            // String-literal twin of `pattern_payload_word_count`'s arm: the
            // debox load must read the `{ ptr, len, cap }` vec struct, not the
            // i64 default. B-2026-08-20-11.
            PatternKind::Literal(LiteralPattern::String(_)) => self.vec_struct_type().into(),
            PatternKind::Tuple(elems) => {
                let elem_tys: Vec<BasicTypeEnum<'ctx>> = elems
                    .iter()
                    .map(|p| self.pattern_payload_llvm_type(p))
                    .collect();
                self.context.struct_type(&elem_tys, false).into()
            }
            // Nested enum-variant sub-pattern — the tagged-union twin of the
            // `pattern_payload_word_count` TupleVariant arm: the debox load
            // must read the inner enum's full `{tag, words…}` shape, not the
            // i64 default (which would load only the first word of the boxed
            // value). Shared enums stay a single RC pointer (B-2026-07-15-5).
            PatternKind::TupleVariant { .. } => {
                if let Some(name) = self.variant_pattern_enum_name(pat) {
                    if self.type_decls.shared_types.contains_key(&name) {
                        return self.context.ptr_type(AddressSpace::default()).into();
                    }
                    if let Some(layout) = self.type_decls.enum_layouts.get(&name) {
                        return layout.llvm_type.into();
                    }
                }
                self.context.i64_type().into()
            }
            PatternKind::Binding(name) => {
                let key = (pat.span.offset, pat.span.length);
                // Unit-variant sub-pattern (`Option.None` nested in another
                // variant's payload) — same rule as the TupleVariant arm
                // above; see the word-count twin for the guard rationale
                // (B-2026-07-15-5).
                if !self.pattern_state.pattern_binding_types.contains_key(&key) {
                    let variant_name = name.rsplit('.').next().unwrap_or(name);
                    if name.contains('.') || self.enum_tag_for_variant(variant_name).is_some() {
                        if let Some(en) = self.variant_pattern_enum_name(pat) {
                            if self.type_decls.shared_types.contains_key(&en) {
                                return self.context.ptr_type(AddressSpace::default()).into();
                            }
                            if let Some(layout) = self.type_decls.enum_layouts.get(&en) {
                                return layout.llvm_type.into();
                            }
                        }
                    }
                }
                // B-2026-07-13-3: monomorph-concrete heap payload type for a
                // generic bare-`T` binding (String/Vec) — the debox `load`
                // needs the full `{ptr,i64,i64}` shape, not the erased i64, or
                // it reads only the box pointer word and zero-pads the rest.
                if let Some(te) = self.mono_payload_binding_type_expr_for(&key) {
                    return self.llvm_type_for_type_expr(&te);
                }
                // Tuple-typed binding: lower the recorded tuple
                // `TypeExpr` to its LLVM struct type so the
                // reconstruction builds a value with the right shape
                // for downstream `let (a, b) = node` destructure.
                if matches!(
                    self.pattern_state
                        .pattern_binding_types
                        .get(&key)
                        .map(|s| s.as_str()),
                    Some("Tuple")
                ) {
                    if let Some(te) = self.pattern_state.pattern_binding_inner_types.get(&key) {
                        if matches!(te.kind, TypeKind::Tuple(_)) {
                            return self.llvm_type_for_type_expr(te);
                        }
                    }
                }
                // `Array[T, N]` / `Vector[T, N]` — the twin of the word-count
                // arm: the debox load must read `[N x T]` / `<N x T>`, not the
                // i64 default (B-2026-08-31-18).
                if matches!(
                    self.pattern_state
                        .pattern_binding_types
                        .get(&key)
                        .map(|s| s.as_str()),
                    Some("Array") | Some("Vector")
                ) {
                    if let Some(te) = self.pattern_state.pattern_binding_inner_types.get(&key) {
                        return self.llvm_type_for_type_expr(te);
                    }
                }
                // Generic user-struct payload binding — the mono aggregate type
                // (sibling of the word-count arm in `pattern_payload_word_count`).
                if let Some(te) = self.pattern_state.pattern_binding_inner_types.get(&key) {
                    if self.is_generic_named_struct_type_expr(te) {
                        return self.llvm_type_for_type_expr(te);
                    }
                }
                match self
                    .pattern_state
                    .pattern_binding_types
                    .get(&key)
                    .map(|s| s.as_str())
                {
                    // `StringSlice` shares `String`'s `{ptr, len, cap}` shape;
                    // `CString` (owning C-string) shares it too.
                    Some("Vec") | Some("VecDeque") | Some("String") | Some("StringSlice")
                    | Some("CString") => self.vec_struct_type().into(),
                    Some("Slice") => self.slice_struct_type().into(),
                    // Float-typed element binding (`Some((x, y))` where the
                    // elements are floats): the tuple slot must be the real
                    // float type, because `reconstruct_payload_value`'s scalar
                    // tail rebuilds the element AS a float (pat.fb.bc) and an
                    // `insertvalue float into i64` slot is invalid IR
                    // (B-2026-07-20-12 companion hardening).
                    Some(n @ ("f16" | "bf16" | "f32" | "f64")) => self.llvm_type_for_name(n),
                    // Shared type (struct OR enum): the value is an RC heap
                    // pointer — a single `ptr`, not the inline tagged-union /
                    // struct aggregate. Must precede the struct/enum arms: a
                    // direct recursive shared enum (`shared enum Wrap { Leaf,
                    // Box(Wrap) }`) binding `Box(inner)` would otherwise return
                    // the by-value `{tag,w0}` and `reconstruct_payload_value`
                    // would `want` 2 words against the 1 stored, take the debox
                    // path, and load a `{i64,i64}` from the pointer — an ICE in
                    // the leaf binder (it expected a pointer).
                    Some(name) if self.type_decls.shared_types.contains_key(name) => {
                        self.context.ptr_type(AddressSpace::default()).into()
                    }
                    Some(name) => self
                        .type_decls
                        .struct_types
                        .get(name)
                        .map(|st| (*st).into())
                        .or_else(|| {
                            // Enum-typed binding: return the enum's tagged-
                            // union LLVM type so `reconstruct_payload_value`
                            // builds a struct of the right shape. Mirrors the
                            // analogous arm in `pattern_payload_word_count`.
                            self.type_decls
                                .enum_layouts
                                .get(name)
                                .map(|layout| layout.llvm_type.into())
                        })
                        .unwrap_or_else(|| self.context.i64_type().into()),
                    None => self.context.i64_type().into(),
                }
            }
            // Struct-pattern payload destructure (slice 3t) — the twin of
            // the `pattern_payload_word_count` arm: the reconstructed value
            // must be typed as the struct's real LLVM aggregate (or the
            // enum's tagged union for a struct-VARIANT pattern), not the
            // i64 default, so the debox load and the field-by-field rebuild
            // both see the right shape.
            PatternKind::Struct { path, .. } => {
                if let Some(enum_name) = self.variant_pattern_enum_name(pat) {
                    if let Some(layout) = self.type_decls.enum_layouts.get(&enum_name) {
                        return layout.llvm_type.into();
                    }
                }
                path.last()
                    .and_then(|n| self.type_decls.struct_types.get(n.as_str()))
                    .map(|st| (*st).into())
                    .unwrap_or_else(|| self.context.i64_type().into())
            }
            _ => self.context.i64_type().into(),
        }
    }

    /// Compound-payload enum codegen (CP4 destructure side helper) —
    /// reconstruct an aggregate `BasicValueEnum` from a sequence of i64
    /// payload words loaded from a variant's payload area. Single-word
    /// fields short-circuit to the legacy single-i64 binding (the
    /// pattern's `Binding` arm already handles struct-payload
    /// reconstitution). Multi-word fields look up the binding's
    /// recorded type via `pattern_binding_types` (set by the
    /// typechecker's `check_pattern_against`) and use the matching LLVM
    /// type to reassemble: 3-word `String` / `Vec[T]` rebuild as
    /// `vec_struct_type` (`{ ptr, i64, i64 }`); 2-word `Slice[T]`
    /// rebuild as `slice_struct_type`; user struct fields rebuild as
    /// the registered LLVM struct type. Tuple sub-patterns dispatch
    /// through a per-element walk that uses `pattern_payload_word_count`
    /// to slice `field_words` and recurses for nested tuples.
    pub(super) fn reconstruct_payload_value(
        &self,
        sub_pat: &Pattern,
        field_words: &[inkwell::values::IntValue<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        // Oversized boxed payload (see `coerce_to_payload_words`): when
        // `T`'s LLVM word count exceeds the payload words we were handed,
        // the pack side heap-boxed it and stored the box pointer in word
        // 0. `inttoptr` it, load `T` back, and re-decompose into its true
        // words so every reconstruction branch below runs identically to
        // the inline path. The predicate `want > field_words.len()` is the
        // unpack mirror of pack's `out.len() > num_words`; pre-boxing,
        // oversized payloads errored at pack and never reached here, so a
        // `want` over the area unambiguously means boxed.
        let want = self.pattern_payload_word_count(sub_pat);
        let deboxed_words;
        let field_words: &[inkwell::values::IntValue<'ctx>] =
            if want > field_words.len() && !field_words.is_empty() {
                let whole_ty = self.pattern_payload_llvm_type(sub_pat);
                let box_ptr = self
                    .builder
                    .build_int_to_ptr(field_words[0], ptr_ty, "enumbox.p")
                    .unwrap();
                let loaded = self
                    .builder
                    .build_load(whole_ty, box_ptr, "enumbox.ld")
                    .unwrap();
                // malloc's guarantee, not the type's natural alignment — an
                // over-aligned payload (`<4 x i64>` wants 32) would lower to a
                // faulting `vmovaps` (B-2026-08-31-18).
                if let Some(inst) = loaded.as_instruction_value() {
                    let _ = inst.set_alignment(8);
                }
                deboxed_words = self.coerce_to_payload_words(loaded, want)?;
                deboxed_words.as_slice()
            } else {
                field_words
            };
        // Tuple sub-pattern: walk per-element, reconstruct each into its
        // own LLVM aggregate (or single word for primitive elements),
        // then pack into a tuple struct value. The element word counts
        // come from `pattern_payload_word_count` which mirrors the
        // construction-side `payload_word_count_for_type_expr` logic on
        // pattern shape (Vec/String=3, Slice=2, struct=struct-fields,
        // primitive/wildcard=1; tuple=sum). Recursive on nested tuples.
        if let PatternKind::Tuple(elems) = &sub_pat.kind {
            let elem_tys: Vec<BasicTypeEnum<'ctx>> = elems
                .iter()
                .map(|p| self.pattern_payload_llvm_type(p))
                .collect();
            let tuple_ty = self.context.struct_type(&elem_tys, false);
            let mut agg = tuple_ty.get_undef();
            let mut cursor = 0usize;
            for (i, sub) in elems.iter().enumerate() {
                let n = self.pattern_payload_word_count(sub);
                let end = (cursor + n).min(field_words.len());
                let slice = &field_words[cursor..end];
                let elem_val = self.reconstruct_payload_value(sub, slice)?;
                // A single-word shared/pointer element (`(shared T, ..)`)
                // reconstructs as the raw i64 payload word, but its tuple slot
                // type is `ptr` (`pattern_payload_llvm_type`'s shared arm). Left
                // as-is this emits `insertvalue i64 into ptr` — invalid IR that
                // fails module verification (a `(shared T, i64)` tuple
                // destructured out of an `Option`, e.g. `Some((current, d)) =
                // stack.pop()`, B-2026-07-08-16). `inttoptr` to match the slot.
                let elem_val = match (elem_tys[i], elem_val) {
                    (BasicTypeEnum::PointerType(pt), BasicValueEnum::IntValue(iv)) => self
                        .builder
                        .build_int_to_ptr(iv, pt, "tup.elem.i2p")
                        .unwrap()
                        .into(),
                    _ => elem_val,
                };
                agg = self
                    .builder
                    .build_insert_value(agg, elem_val, i as u32, "tup.iv")
                    .unwrap()
                    .into_struct_value();
                cursor = end;
            }
            return Ok(agg.into());
        }
        // Nested enum-variant sub-pattern (e.g. the inner `E.A(c)` of
        // `Result.Err(E.A(c))`). Rebuild the inner enum's tagged-union
        // aggregate `{ tag, payload... }` from the payload words so the
        // recursive `bind_pattern_values` can descend into it and bind
        // the inner payload (`c`). Without this the variant pattern falls
        // to the single-word path below and binds the raw tag word,
        // leaving the inner binding unset ("Undefined variable 'c'").
        // (phase-7-codegen.md — nested enum-payload bind.)
        if let Some(enum_ty) = self.enum_layout_type_for_variant_pattern(sub_pat) {
            let n = enum_ty.count_fields() as usize;
            let mut agg = enum_ty.get_undef();
            for i in 0..n {
                let w = field_words
                    .get(i)
                    .copied()
                    .unwrap_or_else(|| i64_t.const_int(0, false));
                agg = self
                    .builder
                    .build_insert_value(agg, w, i as u32, "nested.enum.iv")
                    .unwrap()
                    .into_struct_value();
            }
            return Ok(agg.into());
        }
        // A single-field STRUCT binding (e.g. `Ok(listener)` where
        // `listener: TcpListener { fd: i32 }`) must still reconstitute the
        // struct aggregate, not bind the raw payload word — otherwise the
        // binding slot is sized as `i64` (the word) while `var_type_names`
        // says it's the struct, and the next `.method()` dispatch extracts a
        // struct field from an `i64` and trips module verification. Multi-
        // field structs (e.g. `Response`) already take the struct-building
        // path below because they span >1 word; a struct with one primitive
        // field is the gap. Route any struct-typed binding through that path
        // regardless of word count.
        let binding_is_struct = {
            let key = (sub_pat.span.offset, sub_pat.span.length);
            self.pattern_state.pattern_binding_types
                .get(&key)
                .map(|n| self.type_decls.struct_types.contains_key(n.as_str()))
                .unwrap_or(false)
                // Struct-pattern destructure (slice 3t): a 1-word struct
                // payload (`struct P { x: i64 }`) must still rebuild the
                // `{i64}` aggregate — the raw-word path would hand the
                // struct bind arm an IntValue its guard rejects.
                || matches!(
                    &sub_pat.kind,
                    PatternKind::Struct { path, .. }
                        if path.last().is_some_and(|n| self.type_decls.struct_types.contains_key(n.as_str()))
                )
        };
        // Single-word: keep legacy single-i64 binding shape. The
        // PatternKind::Binding arm handles single-field struct
        // reconstitution downstream via `pattern_binding_types`.
        // Gate on the BINDING's natural width (not the slice length)
        // so widened variant payloads (e.g. the seeded `Option[T]`
        // bumped to 3 i64 payload words to fit tuple/Vec/String
        // payloads from `Vec.pop` / `VecDeque.pop_*`) don't force
        // primitive bindings through the multi-word reconstruction
        // path. The slice may legitimately carry more words than the
        // binding consumes — trailing words are undef.
        let want_words = self.pattern_payload_word_count(sub_pat);
        if (want_words <= 1 || field_words.len() <= 1) && !binding_is_struct {
            let w = field_words
                .first()
                .copied()
                .unwrap_or_else(|| i64_t.const_int(0, false));
            // Sub-64-bit payload narrowing: variant payload words are
            // uniformly i64 in the word stream, but a binding whose
            // surface type is narrower than 64 bits needs a trunc back
            // to its real LLVM width. Two motivating shapes:
            //   - `Json.Bool(b) => b` binding `b: bool` needs i1 so the
            //     `fn -> bool` return path doesn't trip the verifier on
            //     `ret i64 %b`.
            //   - `Vec[u8].pop()`'s `Some(b) => b == other_u8` binds
            //     `b: u8` (i8); without the trunc the comparison emits
            //     `icmp i64 %b, i8 …` and module verification fails with
            //     "Both operands to ICmp instruction are not of the same
            //     type!".
            // The typechecker records the canonical surface name
            // (`"bool"`, `"u8"`, `"i32"`, `"char"`, …) in
            // `pattern_binding_types`; `llvm_type_for_name` maps it back
            // to the LLVM int type, and any width < 64 gets truncated.
            // Width-64 names (`i64`/`u64`/`usize`) and non-int surfaces
            // resolve to a 64-bit or non-`IntType`, so they pass through
            // untouched.
            // A FLOAT literal sub-pattern (`Some(1.5)`) BINDS nothing, so the
            // typechecker records no `pattern_binding_types` entry and the
            // recorded-name block below — which owns every other narrowing and
            // the float bitcast — cannot fire. The raw i64 BIT PATTERN would
            // then reach `compile_pattern_condition`'s float comparison and
            // never equal the literal, turning the B-2026-08-20-11 over-match
            // into an under-match. Take the width from the literal's own
            // suffix instead; the body mirrors the recorded-name `FloatType`
            // arm below (f64 bitcasts whole, narrower floats sit in the low
            // bits and truncate first).
            if let PatternKind::Literal(LiteralPattern::Float(_, sfx)) = &sub_pat.kind {
                let ft = self.llvm_float_type_for_suffix(*sfx);
                let bits_ty = self.float_bits_int_type(ft);
                let src = if bits_ty.get_bit_width() == 64 {
                    w
                } else {
                    self.builder
                        .build_int_truncate(w, bits_ty, "pat.flit.tr")
                        .unwrap()
                };
                return Ok(self.builder.build_bit_cast(src, ft, "pat.flit.bc").unwrap());
            }
            let key = (sub_pat.span.offset, sub_pat.span.length);
            if let Some(name) = self.pattern_state.pattern_binding_types.get(&key).cloned() {
                match self.llvm_type_for_name(&name) {
                    // Wider-than-64-bit payload (`Option[i128]`): the pack side
                    // SPLIT the value across two payload words (lo, hi) because
                    // a word is an i64 — `coerce_to_payload_words`' wide-int
                    // arm. Rejoin them here. Reading word 0 alone bound the low
                    // half, so `Some(2^100)` matched and then printed `0`
                    // (B-2026-08-19-19). Symmetric with the pack split, and
                    // reached only for a `>64`-bit surface name, so every
                    // existing width is untouched.
                    BasicTypeEnum::IntType(it) if it.get_bit_width() > 64 => {
                        let lo = self
                            .builder
                            .build_int_z_extend(w, it, "pat.i128.lo")
                            .unwrap();
                        let joined = match field_words.get(1) {
                            Some(hw) => {
                                let hi = self
                                    .builder
                                    .build_int_z_extend(*hw, it, "pat.i128.hi")
                                    .unwrap();
                                let sh = self
                                    .builder
                                    .build_left_shift(hi, it.const_int(64, false), "pat.i128.shl")
                                    .unwrap();
                                self.builder.build_or(lo, sh, "pat.i128.or").unwrap()
                            }
                            None => lo,
                        };
                        return Ok(joined.into());
                    }
                    BasicTypeEnum::IntType(it) if it.get_bit_width() < 64 => {
                        let narrowed = self
                            .builder
                            .build_int_truncate(w, it, "pat.int.tr")
                            .unwrap();
                        return Ok(narrowed.into());
                    }
                    // Float-typed binding: the payload word carries the float's
                    // bit pattern (packed via `coerce_to_i64`'s bitcast), so it
                    // must be bitcast back — otherwise the binding is the raw
                    // i64 bits and any use (println, arithmetic) reads garbage.
                    // f64 bitcasts directly; f32's pattern sits in the low 32
                    // bits, so truncate then bitcast. Without this, every enum
                    // float payload (`Option[f64]`, the lexer's
                    // `Token::Float(f64, …)`) is corrupt.
                    BasicTypeEnum::FloatType(ft) => {
                        let bits_ty = self.float_bits_int_type(ft);
                        if bits_ty.get_bit_width() == 64 {
                            let f = self.builder.build_bit_cast(w, ft, "pat.f64.bc").unwrap();
                            return Ok(f);
                        } else {
                            // f32 → low 32 bits; f16/bf16 → low 16 bits
                            // (B-2026-07-20-12): truncate to the float's exact
                            // width, then bitcast.
                            let lo = self
                                .builder
                                .build_int_truncate(w, bits_ty, "pat.fb.tr")
                                .unwrap();
                            let f = self.builder.build_bit_cast(lo, ft, "pat.fb.bc").unwrap();
                            return Ok(f);
                        }
                    }
                    _ => {}
                }
            }
            return Ok(w.into());
        }
        // Tuple-typed binding (e.g. `Some(node)` where node: (i64, i64)):
        // walk per-element from the recorded tuple `TypeExpr` and pack
        // into the tuple struct value. Mirrors the Tuple sub-pattern
        // branch above but reads element types from the typechecker
        // side-table instead of sub-pattern shapes.
        let key = (sub_pat.span.offset, sub_pat.span.length);
        if matches!(
            self.pattern_state
                .pattern_binding_types
                .get(&key)
                .map(|s| s.as_str()),
            Some("Tuple")
        ) {
            if let Some(te) = self.pattern_state.pattern_binding_inner_types.get(&key) {
                if let TypeKind::Tuple(elem_tes) = &te.kind {
                    // Slice 3v (leg B): rebuild via the word-correct recursive
                    // helper (#44's `reconstruct_struct_from_words`). The prior
                    // inline walk assumed every element was a single word and
                    // `insertvalue`d a bare i64 into a multi-word slot — a
                    // `Some(x)` binding an `(String, i64)` payload died in
                    // module verification ("Invalid InsertValueInst operands"),
                    // from ANY source (Map.get, Vec.pop, plain bindings).
                    let elem_llvm_tys: Vec<BasicTypeEnum<'ctx>> = elem_tes
                        .iter()
                        .map(|et| self.llvm_type_for_type_expr(et))
                        .collect();
                    let tuple_ty = self.context.struct_type(&elem_llvm_tys, false);
                    let agg = self.reconstruct_struct_from_words(tuple_ty, field_words)?;
                    return Ok(agg.into());
                }
            }
        }
        // `Array[T, N]` / `Vector[T, N]`: neither is a struct, so the
        // field-by-field machinery below (which falls back to
        // `vec_struct_type` for any non-struct target) cannot build it.
        // Delegate to `rebuild_value_from_payload_word_slice`, which owns the
        // array/vector reconstruction the Display path uses — one
        // implementation, so the two backends' unpacks cannot drift
        // (B-2026-08-31-18).
        if matches!(
            self.pattern_state
                .pattern_binding_types
                .get(&key)
                .map(|s| s.as_str()),
            Some("Array") | Some("Vector")
        ) {
            if let Some(te) = self.pattern_state.pattern_binding_inner_types.get(&key) {
                let target = self.llvm_type_for_type_expr(te);
                if matches!(
                    target,
                    BasicTypeEnum::ArrayType(_) | BasicTypeEnum::VectorType(_)
                ) {
                    return self.rebuild_value_from_payload_word_slice(target, field_words);
                }
            }
        }
        // B-2026-08-31-39 — the same two shapes reached through the MONOMORPH
        // channel instead of the typechecker's. Inside `fn show[T](x:
        // Option[T])` the typechecker records nothing at the binding span, so
        // the arm above cannot fire; the concrete `Array[i64, 2]` lives in
        // `mono_payload_binding_type_exprs`. Without this the target lowered to
        // `[2 x i64]`, which is not a `StructType`, and the `st` fallback below
        // silently substituted `vec_struct_type()` — so the arm rebuilt the
        // two inline element words as a `{ptr, len, cap}` String, `inttoptr`'d
        // element 0 and SEGFAULTED in the renderer's memcpy.
        if let Some(te) = self.mono_payload_binding_type_expr_for(&key) {
            let target = self.llvm_type_for_type_expr(&te);
            if matches!(
                target,
                BasicTypeEnum::ArrayType(_) | BasicTypeEnum::VectorType(_)
            ) {
                return self.rebuild_value_from_payload_word_slice(target, field_words);
            }
        }
        // Multi-word: resolve the binding's surface type to choose the
        // target LLVM aggregate type. A Struct-PATTERN sub-pattern (slice
        // 3t: `Some(Holder { name, id })`) has no `pattern_binding_types`
        // entry at its own span — its name comes from the pattern path, and
        // the existing field-by-field rebuild below (incl. the #44 nested
        // sub-field recursion) then produces the correctly-typed aggregate
        // for `bind_pattern_values`' plain-struct destructure arm.
        let type_name = self
            .pattern_state
            .pattern_binding_types
            .get(&key)
            .cloned()
            .or_else(|| match &sub_pat.kind {
                PatternKind::Struct { path, .. } => path
                    .last()
                    .filter(|n| self.type_decls.struct_types.contains_key(n.as_str()))
                    .cloned(),
                _ => None,
            });
        // A generic user-struct payload binding rebuilds into its MONO
        // aggregate (the concrete-arg field layout), NOT the all-`i64` generic
        // base `struct_types[name]` the fallthrough would pick — else the
        // 3-word `String` field collapses into the 1-field base (B-2026-07-12-2).
        let mono_struct_target: Option<BasicTypeEnum<'ctx>> = self
            .pattern_state
            .pattern_binding_inner_types
            .get(&key)
            .filter(|te| self.is_generic_named_struct_type_expr(te))
            .map(|te| self.llvm_type_for_type_expr(te));
        // B-2026-07-13-3: a generic enum's bare-`T` payload resolved to a
        // concrete heap type by the monomorph substitution (String/Vec, OR a
        // user struct / enum wider than the erased 1-word area). Rebuild at that
        // exact aggregate — the `field_words.len()`-based heuristic below only
        // knows the 3-word→vec / 2-word→slice shapes, so a user-struct payload
        // (`enum Opt[T] { Yes(T) }` at `T = struct Box { s: String }`) would
        // otherwise rebuild as `{ptr,i64,i64}` instead of `{ {ptr,i64,i64} }`
        // and the arm value would disagree with the return type (`ret i64 0`).
        let mono_target: Option<BasicTypeEnum<'ctx>> = self
            .mono_payload_binding_type_expr_for(&key)
            .map(|te| self.llvm_type_for_type_expr(&te));
        let target_ty: Option<BasicTypeEnum<'ctx>> =
            mono_struct_target.or(mono_target).or_else(|| {
                type_name.as_ref().and_then(|n| match n.as_str() {
                    "String" | "str" | "Vec" | "VecDeque" | "StringSlice" | "CString" => {
                        Some(self.vec_struct_type().into())
                    }
                    "Slice" => Some(self.slice_struct_type().into()),
                    _ => self
                        .type_decls
                        .struct_types
                        .get(n.as_str())
                        .map(|st| (*st).into())
                        // Enum-typed binding (e.g. `Ok(j)` where j: Json):
                        // return the enum's tagged-union LLVM struct so the
                        // multi-word destructure rebuilds a `{tag, w0..wN}`
                        // value the downstream method-call dispatcher can
                        // operate on. Without this, the heuristic fallback
                        // below picks `vec_struct_type` (`{ptr, i64, i64}`)
                        // and downstream `.method()` calls explode when
                        // they extract the tag from field 0 as a pointer.
                        .or_else(|| {
                            self.type_decls
                                .enum_layouts
                                .get(n.as_str())
                                .map(|layout| layout.llvm_type.into())
                        }),
                })
            });
        // Heuristic fallback when the typechecker didn't record a name:
        // 3 words → vec/string shape; 2 words → slice shape.
        let target_ty: BasicTypeEnum<'ctx> = target_ty.unwrap_or_else(|| match field_words.len() {
            3 => self.vec_struct_type().into(),
            2 => self.slice_struct_type().into(),
            _ => self.vec_struct_type().into(),
        });
        let st = match target_ty {
            BasicTypeEnum::StructType(s) => s,
            _ => self.vec_struct_type(),
        };
        let mut agg = st.get_undef();
        // Reconstruct field-by-field. Each LLVM field of the target
        // struct consumes `llvm_type_word_count(field_ty)` i64 words
        // from `field_words` in source-declaration order (matches
        // `coerce_to_payload_words`'s decomposition shape). Primitive
        // fields (int/float/ptr) consume 1 word; nested struct fields
        // (e.g. a `String` aggregate `{ ptr, i64, i64 }` inside a
        // `Response { status: i64, body: String }` payload) consume
        // their full field width and rebuild sub-by-sub.
        let n_fields = st.count_fields() as usize;
        let mut cursor = 0usize;
        for i in 0..n_fields {
            let field_ty = st
                .get_field_type_at_index(i as u32)
                .ok_or_else(|| format!("field type at index {} missing", i))?;
            let n = Self::llvm_type_word_count(field_ty).max(1);
            let end = (cursor + n).min(field_words.len());
            if cursor >= field_words.len() {
                break;
            }
            let slice = &field_words[cursor..end];
            // #37: a single-WORD struct field (e.g. `{i64}` — a unit-only enum
            // like `BinOp` used as a payload-struct field) has `n == 1` but its
            // LLVM type is a `StructType`, so it must reconstruct via the
            // struct branch below (wrap the word in the `{i64}` aggregate), NOT
            // the scalar path (which would `insertvalue` a bare `i64` into a
            // `{i64}` slot → "Invalid InsertValueInst operands"). Exclude struct
            // field types from the scalar branch so they fall through.
            let field_val: BasicValueEnum<'ctx> =
                if n == 1 && !matches!(field_ty, BasicTypeEnum::StructType(_)) {
                    let word = slice
                        .first()
                        .copied()
                        .unwrap_or_else(|| i64_t.const_int(0, false));
                    match field_ty {
                        BasicTypeEnum::IntType(it) => {
                            if it.get_bit_width() == 64 {
                                word.into()
                            } else if it.get_bit_width() < 64 {
                                self.builder
                                    .build_int_truncate(word, it, "pl.tr")
                                    .unwrap()
                                    .into()
                            } else {
                                self.builder
                                    .build_int_z_extend(word, it, "pl.zx")
                                    .unwrap()
                                    .into()
                            }
                        }
                        BasicTypeEnum::FloatType(ft) => {
                            // f64 bitcasts directly; a narrower float unpacks
                            // from the word's low bits at its exact width
                            // (B-2026-07-20-11 f32, B-2026-07-20-12 f16/bf16).
                            let bits_ty = self.float_bits_int_type(ft);
                            if bits_ty.get_bit_width() == 64 {
                                self.builder.build_bit_cast(word, ft, "pl.fc").unwrap()
                            } else {
                                let lo = self
                                    .builder
                                    .build_int_truncate(word, bits_ty, "pl.fb.tr")
                                    .unwrap();
                                self.builder.build_bit_cast(lo, ft, "pl.fb.bc").unwrap()
                            }
                        }
                        BasicTypeEnum::PointerType(_) => self
                            .builder
                            .build_int_to_ptr(word, ptr_ty, "pl.itop")
                            .unwrap()
                            .into(),
                        _ => word.into(),
                    }
                } else if matches!(
                    field_ty,
                    BasicTypeEnum::ArrayType(_) | BasicTypeEnum::VectorType(_)
                ) {
                    // An `Array[T, N]` / `Vector[T, N]` FIELD spans N element
                    // words. Before B-2026-08-31-18 taught `llvm_type_word_count`
                    // about vectors, such a field reported one word and fell to
                    // the scalar branch above; with the true width it reached the
                    // "unexpected multi-word non-struct" fallback below, which
                    // keeps word 0 and `insertvalue`s a bare `i64` into a
                    // `<4 x i64>` slot — invalid IR that fails module
                    // verification. Delegate to the shared rebuilder, which owns
                    // both shapes.
                    self.rebuild_value_from_payload_word_slice(field_ty, slice)?
                } else if let BasicTypeEnum::StructType(inner_st) = field_ty {
                    // Nested struct field — rebuild via the recursive,
                    // word-correct helper. #44 (phase-12 parser slice 2a): the
                    // prior inline walk assumed every sub-field was a SINGLE
                    // word (`slice[j]`) and `insertvalue`d a bare `i64` into a
                    // multi-word struct sub-field (`Vec {ptr,i64,i64}` /
                    // `Option {4×i64}` / `Span {4×i64}` inside the parser's
                    // `Block` payload struct, reached through `IfExpr.then_block:
                    // Block`) → "Invalid InsertValueInst operands". The helper
                    // walks sub-fields by their true `llvm_type_word_count` width
                    // and recurses into nested structs.
                    self.reconstruct_struct_from_words(inner_st, slice)?.into()
                } else {
                    // Unexpected: a multi-word non-struct field. Fall back to
                    // dropping all but the first word — same shape as the
                    // legacy single-word path so we don't crash the build.
                    let word = slice
                        .first()
                        .copied()
                        .unwrap_or_else(|| i64_t.const_int(0, false));
                    word.into()
                };
            agg = self
                .builder
                .build_insert_value(agg, field_val, i as u32, "pl.iv")
                .unwrap()
                .into_struct_value();
            cursor = end;
        }
        Ok(agg.into())
    }

    /// Reconstruct a struct VALUE from its flat `i64`-word decomposition,
    /// recursing into nested struct fields. Each LLVM field consumes
    /// `llvm_type_word_count(field_ty)` words in declaration order (the inverse
    /// of `coerce_to_payload_words`). #44 (phase-12 parser slice 2a): the
    /// previous inline nested-struct reconstruction in `reconstruct_payload_value`
    /// assumed every sub-field was a single word and `insertvalue`d a bare `i64`
    /// into a multi-word struct sub-field (`Vec {ptr,i64,i64}` / `Option {4×i64}`
    /// / `Span {4×i64}` inside a `Block` payload struct reached via
    /// `IfExpr.then_block: Block`) → "Invalid InsertValueInst operands". This
    /// walks sub-fields by their true word width and recurses, so an arbitrarily
    /// nested heap-bearing payload struct rebuilds correctly.
    pub(super) fn reconstruct_struct_from_words(
        &self,
        st: inkwell::types::StructType<'ctx>,
        words: &[inkwell::values::IntValue<'ctx>],
    ) -> Result<inkwell::values::StructValue<'ctx>, String> {
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_t = self.context.i64_type();
        let mut agg = st.get_undef();
        let n_fields = st.count_fields() as usize;
        let mut cursor = 0usize;
        for i in 0..n_fields {
            let field_ty = st
                .get_field_type_at_index(i as u32)
                .ok_or_else(|| format!("sub-field type at index {} missing", i))?;
            let n = Self::llvm_type_word_count(field_ty).max(1);
            if cursor >= words.len() {
                break;
            }
            let end = (cursor + n).min(words.len());
            let slice = &words[cursor..end];
            let field_val: BasicValueEnum<'ctx> = if let BasicTypeEnum::StructType(inner) = field_ty
            {
                self.reconstruct_struct_from_words(inner, slice)?.into()
            } else if matches!(
                field_ty,
                BasicTypeEnum::ArrayType(_) | BasicTypeEnum::VectorType(_)
            ) {
                // Twin of the array/vector branch in `reconstruct_payload_value`
                // (B-2026-08-31-18) — this helper is the one a NESTED struct
                // field recurses through, so both need it or a vector one level
                // down still collapses to word 0.
                self.rebuild_value_from_payload_word_slice(field_ty, slice)?
            } else {
                let word = slice
                    .first()
                    .copied()
                    .unwrap_or_else(|| i64_t.const_int(0, false));
                match field_ty {
                    BasicTypeEnum::IntType(it) => {
                        if it.get_bit_width() == 64 {
                            word.into()
                        } else if it.get_bit_width() < 64 {
                            self.builder
                                .build_int_truncate(word, it, "pl.sub.tr")
                                .unwrap()
                                .into()
                        } else {
                            self.builder
                                .build_int_z_extend(word, it, "pl.sub.zx")
                                .unwrap()
                                .into()
                        }
                    }
                    BasicTypeEnum::FloatType(ft) => {
                        // Same exact-width rule as `pl.fc` above
                        // (B-2026-07-20-11 f32, B-2026-07-20-12 f16/bf16).
                        let bits_ty = self.float_bits_int_type(ft);
                        if bits_ty.get_bit_width() == 64 {
                            self.builder.build_bit_cast(word, ft, "pl.sub.fc").unwrap()
                        } else {
                            let lo = self
                                .builder
                                .build_int_truncate(word, bits_ty, "pl.sub.fb.tr")
                                .unwrap();
                            self.builder.build_bit_cast(lo, ft, "pl.sub.fb.bc").unwrap()
                        }
                    }
                    BasicTypeEnum::PointerType(_) => self
                        .builder
                        .build_int_to_ptr(word, ptr_ty, "pl.sub.itop")
                        .unwrap()
                        .into(),
                    _ => word.into(),
                }
            };
            agg = self
                .builder
                .build_insert_value(agg, field_val, i as u32, "pl.sub.iv")
                .unwrap()
                .into_struct_value();
            cursor = end;
        }
        Ok(agg)
    }

    /// After a `match scrut { Variant(b1, b2, …) => … }` arm has bound
    /// the variant payload fields, suppress the source enum's
    /// scope-exit cleanup for any payload field whose binding now
    /// owns a heap buffer. Concretely: for each pattern position
    /// whose `EnumDropKind` is `VecOrString` and whose sub-pattern is
    /// a value-consuming `Binding`, zero the cap word in the source
    /// enum's alloca. The `__karac_drop_<E>` runtime walk reads
    /// `cap > 0` per heap-bearing field and skips the `free` when
    /// the guard is false — same shape `CleanupAction::FreeVecBuffer`
    /// uses for plain Vec / String bindings at the let-site.
    ///
    /// No-op when: scrutinee isn't a simple identifier (we can't
    /// locate the source alloca), the binding's type isn't a
    /// non-shared enum with a known layout, or the arm's pattern
    /// isn't `TupleVariant`. The arm-body's compiled cleanup walk
    /// (`drain_top_frame_with_emit`) freeing the new binding stays
    /// load-bearing — this fn only neutralizes the *source's* drop.
    pub(super) fn suppress_destructured_enum_payload_cleanup(
        &mut self,
        scrutinee: &Expr,
        pattern: &Pattern,
    ) {
        // An owned `self` receiver (`impl E { fn get(self) { match self { E.V(s)
        // => … } } }`) parses as `SelfValue`, not `Identifier("self")`. Without
        // the `SelfValue` arm, a method matching its OWNED enum `self` never
        // cap-zeroed the consumed payload in the source, so `self`'s callee-owned
        // enum-drop (registered by `make_aggregate_param_callee_owned` at entry)
        // freed the payload buffer AND the moved-out binding (or its downstream
        // consumer) freed it again — a double-free (the free-fn `match e { … }`
        // form already worked via the Identifier arm). The enum-payload analogue
        // of B-2026-07-18-37 (self.field move-out). B-2026-07-18-47.
        let scrut_name = match &scrutinee.kind {
            ExprKind::Identifier(n) => n.as_str(),
            ExprKind::SelfValue => "self",
            _ => return,
        };
        let slot = match self.variables.get(scrut_name) {
            Some(s) => *s,
            None => return,
        };
        let enum_name = match self.var_types.var_type_names.get(scrut_name) {
            Some(n) => n.clone(),
            None => return,
        };
        self.suppress_destructured_enum_payload_cleanup_at(slot.ptr, &enum_name, pattern);
        // B-2026-08-31-23 — when the scrutinee is a whole-payload binding over
        // a BOXED payload, the line above disarmed a BIT COPY. The box still
        // owns the same buffer, so disarm there too or the arm's move and the
        // box's drop both free it. No-op for every other scrutinee.
        self.suppress_aliased_boxed_payload_cleanup(scrut_name, pattern);
        // B-2026-07-30-11 (enum leg) — the BODIES channel's half of the same
        // move-out. The cap-zeroing above disarms the source's MEMORY drop; the
        // payload's user `impl Drop` body rides the separate NLL `UserDrop`
        // channel and needs its own retraction, prefix-keyed so an enum with
        // its own `impl Drop` keeps that body. Reached only from an identifier
        // / `self` scrutinee, which is exactly where a let-site registration
        // could exist — a fresh temp has no binding name and never registered
        // one.
        if self.enum_pattern_consumes_user_drop_payload(&enum_name, pattern) {
            self.suppress_container_elem_bodies_for_var(scrut_name);
        }
    }

    /// #15 companion: when a `match` scrutinee is a struct FIELD whose type is a
    /// heap-bearing user enum (`match spanned.tok { Ident(name) => … }`, the
    /// bootstrap's `SpannedToken` shape), the owning struct's synthesized drop
    /// now frees that enum field (`emit_struct_drop_synthesis`'s `EnumField`
    /// arm). For each payload field the arm's pattern *consumes*, cap-zero it
    /// WITHIN the source struct's slot so the struct drop's `__karac_drop_<E>`
    /// walk skips the buffer the binding now owns. Without this, the source
    /// struct and the moved-out binding BOTH free the payload → double-free
    /// (the failure was latent pre-#15 only because struct drop ignored enum
    /// fields entirely). The identifier / fresh-temp suppression above
    /// neutralizes only the scrutinee *copy*, never the field in the source
    /// struct. Handles an arbitrary-depth field-access chain (`ident.f1.f2…`)
    /// rooted at a local binding via [`Self::field_chain_place_ptr`] — `s.tok`
    /// (#15) and `w.sp.tok` (#18's nested `Wrap { sp: Span { tok } }`) alike. A
    /// non-struct hop (mid-chain tuple index, call-rooted base, unresolved type)
    /// no-ops to the status quo.
    pub(super) fn suppress_destructured_struct_field_enum_cleanup(
        &mut self,
        scrutinee: &Expr,
        pattern: &Pattern,
    ) {
        // #15/#18 reach a named struct field (`s.tok`, `w.sp.tok`); #21 adds the
        // tuple-index scrutinee (`match h.pe.0`) — both resolve through the
        // place-chain walkers below, which now handle a `TupleIndex` hop.
        if !matches!(
            &scrutinee.kind,
            ExprKind::FieldAccess { .. } | ExprKind::TupleIndex { .. }
        ) {
            return;
        }
        // The leaf field's declared type must be a (non-shared) user enum — the
        // only kind `emit_struct_drop_synthesis` frees as a struct field.
        // `place_chain_type_name` (not the shared `type_name_of_expr`) so a
        // `vec[i]`-rooted chain (`toks[j].tok`) resolves to the element's enum.
        let Some(enum_name) = self.place_chain_type_name(scrutinee) else {
            return;
        };
        if !self.type_decls.enum_layouts.contains_key(&enum_name) {
            return;
        }
        let Some(field_ptr) = self.field_chain_place_ptr(scrutinee) else {
            return;
        };
        self.suppress_destructured_enum_payload_cleanup_at(field_ptr, &enum_name, pattern);
        // B-2026-08-29-33 — the BODIES half, beside the MEMORY half above.
        // Gated on the arm actually taking a body-running payload, exactly as
        // the identifier path gates its `suppress_container_elem_bodies_for_var`
        // call: with nothing taken there is nothing to hand over, and masking
        // would lose the body outright.
        if self.enum_pattern_consumes_user_drop_payload(&enum_name, pattern) {
            self.disarm_projection_enum_payload_bodies(scrutinee);
        }
    }

    /// #16: a plain struct-pattern match destructure of an OWNED local struct
    /// (`match v { S { a, b: _ } => … }`). Each *consumed* field's heap payload
    /// is moved by value into the new binding (extracted via `build_extract_value`
    /// in `bind_pattern_values`), whose per-arm cleanup frees it; the source
    /// struct's `__karac_drop_<S>` (queued by `track_struct_var` at the let-site)
    /// would re-free the SAME buffer at the outer scope's drain → double-free.
    /// Cap-zero each consumed field's heap caps in the source slot so the struct
    /// drop's `cap > 0` guard skips it — the struct-pattern twin of
    /// [`Self::suppress_destructured_enum_payload_cleanup_at`] (which fires only
    /// for enum `TupleVariant` patterns) and the destructure dual of #14/#19's
    /// `suppress_struct_field_move_into_literal` (the `let m = v.s` field-move-out
    /// path). Unconsumed (`_` / `..`-elided) fields keep their cap and are freed
    /// by the drop walk.
    ///
    /// A consumed field is suppressed only when the field pattern moves the WHOLE
    /// field into a single binding (shorthand `field`, or `field: name`): then the
    /// binding owns the field's entire heap subtree, so zeroing all of its caps
    /// transitively (`zero_enum_payload_caps` / `zero_struct_move_caps`, mirroring
    /// the per-field arms of `zero_struct_move_caps`) is exactly right. A NESTED
    /// destructure (`field: Inner { x }`, `field: (a, _)`) or an `@`-binding is
    /// left to the status quo (no suppression): it consumes only part of the
    /// field, so transitively zeroing the whole field would orphan the unbound
    /// remainder into a leak — bailing keeps the pre-existing behavior (the
    /// nested partial-move is rare and never the bootstrap shape, which binds flat
    /// `.field`s). Bails entirely (status quo, never a regression) on a
    /// ref-scrutinee (caller handles via `scrut_ref_ptr.is_none()` gate),
    /// a non-place scrutinee, an enum struct-variant pattern (its payload is bound
    /// via the word-extract path, not field GEP — the resolved name is the enum,
    /// absent from `struct_field_type_names`), or a shared (RC) struct.
    pub(super) fn suppress_destructured_struct_pattern_cleanup(
        &mut self,
        scrutinee: &Expr,
        pattern: &Pattern,
    ) {
        // Pattern gate BEFORE the place walk — `field_chain_place_ptr` emits
        // IR for a `vec[i]` root (element GEP + bounds check), which must not
        // appear for arms this suppression can never apply to.
        if !matches!(&pattern.kind, PatternKind::Struct { .. }) {
            return;
        }
        let Some(struct_name) = self.place_chain_type_name(scrutinee) else {
            return;
        };
        if self.type_decls.shared_types.contains_key(&struct_name) {
            return;
        }
        let Some(base_ptr) = self.field_chain_place_ptr(scrutinee) else {
            return;
        };
        self.suppress_destructured_struct_pattern_cleanup_at(base_ptr, &struct_name, pattern);
    }

    /// Core of [`Self::suppress_destructured_struct_pattern_cleanup`], keyed on
    /// the source struct's pointer + name directly rather than resolving them
    /// from the scrutinee place expression. The B-2026-07-21-7 ref-chain clone
    /// path calls this with its clone alloca (the place walker bails on the
    /// borrowed root by design), the #16 owned-place path via the resolving
    /// wrapper above.
    pub(super) fn suppress_destructured_struct_pattern_cleanup_at(
        &mut self,
        base_ptr: PointerValue<'ctx>,
        struct_name: &str,
        pattern: &Pattern,
    ) {
        let PatternKind::Struct { fields, .. } = &pattern.kind else {
            return;
        };
        if self.type_decls.shared_types.contains_key(struct_name) {
            return;
        }
        let Some(field_type_names) = self
            .type_decls
            .struct_field_type_names
            .get(struct_name)
            .cloned()
        else {
            return;
        };
        let Some(field_names) = self.type_decls.struct_field_names.get(struct_name).cloned() else {
            return;
        };
        let Some(&st) = self.type_decls.struct_types.get(struct_name) else {
            return;
        };
        let vec_ty = self.vec_struct_type();
        let zero = self.context.i64_type().const_int(0, false);
        for field_pat in fields {
            // Only a whole-field move into one binding is safely suppressible —
            // see the doc comment. A nested destructure / `@`-binding bails.
            let whole_move = match &field_pat.pattern {
                None => true,
                Some(p) => matches!(p.kind, PatternKind::Binding(_)),
            };
            if !whole_move {
                // B-2026-08-31-30 — a NESTED struct sub-pattern
                // (`match q { Q { h: H { r, .. } } => … }`) moves a field of the
                // INNER struct out, and the source's drop still freed it: a
                // DOUBLE FREE, not the quiet duplicate body the flat spelling
                // produced. Suppressing the whole outer field would be wrong
                // (the field itself did not move, only something inside it), so
                // descend one level and run the same per-field zeroing against
                // the inner struct's slot. Recursion, so it composes to any
                // depth.
                if let Some(sub) = &field_pat.pattern {
                    if matches!(sub.kind, PatternKind::Struct { .. }) {
                        let sub = sub.clone();
                        if let Some(idx) = field_names.iter().position(|n| n == &field_pat.name) {
                            let inner = field_type_names.get(idx).and_then(|o| o.clone());
                            if let Some(inner) = inner {
                                if self.type_decls.struct_types.contains_key(inner.as_str())
                                    && !self.type_decls.shared_types.contains_key(inner.as_str())
                                {
                                    if let Ok(fp) = self.builder.build_struct_gep(
                                        st,
                                        base_ptr,
                                        idx as u32,
                                        "p16.nest.p",
                                    ) {
                                        self.suppress_destructured_struct_pattern_cleanup_at(
                                            fp, &inner, &sub,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                continue;
            }
            let Some(idx) = field_names.iter().position(|n| n == &field_pat.name) else {
                continue;
            };
            let fname = field_type_names
                .get(idx)
                .and_then(|o| o.as_deref())
                .unwrap_or("");
            let Ok(field_ptr) =
                self.builder
                    .build_struct_gep(st, base_ptr, idx as u32, "p16.fld.p")
            else {
                continue;
            };
            if matches!(fname, "Vec" | "VecDeque" | "String") {
                if let Ok(cap_ptr) =
                    self.builder
                        .build_struct_gep(vec_ty, field_ptr, 2, "p16.fld.cap")
                {
                    let _ = self.builder.build_store(cap_ptr, zero);
                }
            } else if fname == "Option" {
                // B-2026-07-03-28 Facet A — a match-destructured `Option[heap]`
                // field: the arm's own binding frees the payload, so zero the
                // SOURCE tag and let the struct-drop `OptionInline` free skip it.
                self.zero_option_field_tag_at(field_ptr);
            } else if fname == "Result" {
                // B-2026-07-21-15 — the Result sibling: the struct drop now
                // frees a direct-String/Vec-halves Result field, so a
                // destructured-out field must zero the source's payload area
                // (no-op for wider Result shapes — no registered free).
                if let Some(layout) = self.type_decls.enum_layouts.get("Result") {
                    let result_ty = layout.llvm_type;
                    self.zero_result_payload_area(result_ty, field_ptr, "p16.res");
                }
            } else {
                if let Some(layout) = self.type_decls.enum_layouts.get(fname).cloned() {
                    if !layout.is_shared {
                        self.zero_enum_payload_caps(field_ptr, &layout);
                    }
                } else if self.type_decls.struct_types.contains_key(fname)
                    && !self.type_decls.shared_types.contains_key(fname)
                {
                    self.zero_struct_move_caps(field_ptr, fname);
                }
            }
        }
        // B-2026-08-06-31 — the MATCH-ARM half of the destructure mirror; the
        // `let`-statement half lives in `finish_owned_struct_destructure`, and
        // the self-host parser needs both. `base_ptr` may be an arm binding
        // DEBOXED out of an enum payload box — this frame's private copy of a
        // box the CALLER owns — in which case the zeroing above lands where
        // that owner cannot see it and its drop frees a buffer this destructure
        // just handed to a field binding. Recursing on the box reuses the same
        // per-field walk, so a field the pattern did NOT consume keeps its live
        // cap and the owner still frees it. One level: the box pointer is a
        // fresh `inttoptr` and is never itself a key.
        if let Some(box_ptr) = self
            .payload_vars
            .deboxed_payload_box_ptrs
            .get(&base_ptr)
            .copied()
        {
            self.suppress_destructured_struct_pattern_cleanup_at(box_ptr, struct_name, pattern);
        }
    }

    /// Compute the in-place pointer to a place expression rooted at a local
    /// binding (`ident`, `ident.f`, `ident.f.g`, …), `self`, or a `vec[i]`
    /// element (`toks[j].tok` — B-2026-06-12-6 gap 2), GEP'ing through each
    /// intermediate struct. Returns `None` for any non-struct hop (a tuple index
    /// in the middle, a call-rooted base, an unresolved type) so callers no-op to
    /// the status quo. The leaf pointer addresses the field IN PLACE within its
    /// owning slot — used by the #18 struct-field-enum match suppression to reach
    /// a (possibly deeply nested) enum field in its source, including an enum
    /// field of a Vec element whose buffer the Vec's own drop now frees.
    pub(super) fn field_chain_place_ptr(&mut self, expr: &Expr) -> Option<PointerValue<'ctx>> {
        match &expr.kind {
            // A `ref`/`mut ref` param root (incl. a `ref self` receiver): the
            // variable slot holds a POINTER to the caller's value, not the
            // aggregate itself — GEPing the slot as if it were the struct
            // writes past the 8-byte alloca into adjacent stack slots
            // (B-2026-07-21-5/-6: the match move-out suppression zeroed a
            // neighbouring String binding / corrupted locals). Bail instead of
            // dereferencing: every suppression caller must NOT mutate storage
            // the callee doesn't own (the borrowed source's owner drops it),
            // and the remaining callers all treat `None` as "leave the status
            // quo". A caller needing a real place pointer through a borrow
            // loads the slot explicitly (`mem_place_ptr_and_value`'s ref-param
            // arm is the model).
            ExprKind::Identifier(name) => {
                if self.borrow_vars.ref_params.contains_key(name.as_str()) {
                    return None;
                }
                self.variables.get(name.as_str()).map(|s| s.ptr)
            }
            ExprKind::SelfValue => {
                if self.borrow_vars.ref_params.contains_key("self") {
                    return None;
                }
                self.variables.get("self").map(|s| s.ptr)
            }
            // `vec[i]` root — the in-place element slot inside the Vec buffer, so
            // the suppression reaches an enum field of a Vec ELEMENT, not just a
            // local struct's field. Restricted to a plain (non-array-slot) Vec
            // variable indexed by a side-effect-free index (identifier / int
            // literal): `vec_index_elem_ptr` re-evaluates the index to recompute
            // the element pointer, and a pure index makes that re-eval a no-op —
            // the scrutinee's own `compile_expr` already emitted the
            // authoritative bounds check for the very same index value.
            ExprKind::Index { object, index } => {
                let ExprKind::Identifier(vec_var) = &object.kind else {
                    return None;
                };
                if !matches!(index.kind, ExprKind::Identifier(_) | ExprKind::Integer(..)) {
                    return None;
                }
                // B-2026-08-15-21 — a `mut Slice[T]` container. The registrar
                // for a `Slice[T]` param writes `slice_elem_types` and returns
                // BEFORE the `vec_elem_types` insert, so gating this arm on
                // `vec_elem_types` alone answered "not an indexable place" for
                // every slice — `s[0].x = v` then declined here, declined at
                // both fallbacks in `nested_store_place_ptr` (which admit only
                // field-rooted and impure-index shapes), and left
                // `compile_field_store` through its no-op tail. The write was
                // SILENTLY DROPPED: no store was emitted at all, so even an
                // immediate `s[0].x` read-back inside the same function saw the
                // stale value, and the caller's buffer was never touched.
                //
                // The two neighbouring stores worked and hid this: a WHOLE
                // element (`s[0] = P { .. }`) never reaches `compile_field_store`,
                // and a `shared` element is caught by the indexed-shared branch
                // upstream, which has consulted `slice_elem_types` all along.
                // Plain-struct elements were the gap.
                //
                // Same no-op-tail family as B-2026-08-01-35 (field-rooted
                // container) and B-2026-08-02-15 (impure index) — one more
                // receiver shape no resolver covered.
                if self
                    .var_types
                    .slice_elem_types
                    .contains_key(vec_var.as_str())
                {
                    let vec_var = vec_var.clone();
                    return self
                        .lower_indexed_elem_ptr_slice(&vec_var, index)
                        .ok()
                        .map(|(p, _)| p);
                }
                if !self.var_types.vec_elem_types.contains_key(vec_var.as_str()) {
                    return None;
                }
                // Array-slot Vec bindings have a distinct representation —
                // mirror the bypass in `ref_arg_index_borrow_ptr`.
                let slot_is_array = self
                    .variables
                    .get(vec_var.as_str())
                    .is_some_and(|s| matches!(s.ty, BasicTypeEnum::ArrayType(_)));
                if slot_is_array {
                    return None;
                }
                let vec_var = vec_var.clone();
                self.vec_index_elem_ptr(&vec_var, index).ok()
            }
            ExprKind::FieldAccess { object, field } => {
                let base_ptr = self.field_chain_place_ptr(object)?;
                let obj_ty = self.place_chain_type_name(object)?;
                let st = *self.type_decls.struct_types.get(obj_ty.as_str())?;
                let idx = self
                    .type_decls
                    .struct_field_names
                    .get(obj_ty.as_str())?
                    .iter()
                    .position(|n| n == field)? as u32;
                self.builder
                    .build_struct_gep(st, base_ptr, idx, "match.chain.enum.p")
                    .ok()
            }
            // #21 — a tuple-index hop (`<place>.field.N`, the `match h.pe.0`
            // scrutinee). GEP element `index` of the tuple at `object` in place.
            ExprKind::TupleIndex { object, index } => {
                let base_ptr = self.field_chain_place_ptr(object)?;
                let tuple_ty = self.place_chain_aggregate_llvm_type(object)?;
                self.builder
                    .build_struct_gep(tuple_ty, base_ptr, *index as u32, "match.chain.tupidx.p")
                    .ok()
            }
            _ => None,
        }
    }

    /// LLVM aggregate (struct/tuple) type of a place expression — used by the
    /// tuple-index arm of [`Self::field_chain_place_ptr`] to GEP a tuple element.
    /// `&self` — pure type lookup, no IR.
    pub(super) fn place_chain_aggregate_llvm_type(&self, expr: &Expr) -> Option<StructType<'ctx>> {
        match &expr.kind {
            ExprKind::Identifier(n) => match self.variables.get(n.as_str())?.ty {
                BasicTypeEnum::StructType(t) => Some(t),
                _ => None,
            },
            ExprKind::SelfValue => match self.variables.get("self")?.ty {
                BasicTypeEnum::StructType(t) => Some(t),
                _ => None,
            },
            ExprKind::FieldAccess { object, field } => {
                let obj_ty = self.place_chain_type_name(object)?;
                let st = *self.type_decls.struct_types.get(obj_ty.as_str())?;
                let idx = self
                    .type_decls
                    .struct_field_names
                    .get(obj_ty.as_str())?
                    .iter()
                    .position(|n| n == field)? as u32;
                match st.get_field_type_at_index(idx)? {
                    BasicTypeEnum::StructType(t) => Some(t),
                    _ => None,
                }
            }
            ExprKind::TupleIndex { object, index } => {
                let outer = self.place_chain_aggregate_llvm_type(object)?;
                match outer.get_field_type_at_index(*index as u32)? {
                    BasicTypeEnum::StructType(t) => Some(t),
                    _ => None,
                }
            }
            // B-2026-08-02-8 — a `vec[i]` receiver (`v[0].0 = x`): the
            // element's LLVM aggregate type from the container's recorded
            // element type. `field_chain_place_ptr` already resolves the
            // element POINTER for this shape; without this arm the
            // tuple-element store loud-bailed at the aggregate lookup.
            //
            // B-2026-08-15-24 — and a `slice[i]` receiver, for the same
            // reason. A `Slice[T]` param registers `slice_elem_types` and not
            // `vec_elem_types` (its registrar returns before that insert), so
            // consulting only the Vec table answered `None` for every slice
            // and `s[0].0 = x` loud-bailed here even after B-2026-08-15-21
            // taught `field_chain_place_ptr` to hand back the element pointer.
            // The pointer half and the type half had to learn about slices
            // separately; this is the second half.
            ExprKind::Index { object, .. } => {
                let ExprKind::Identifier(v) = &object.kind else {
                    return None;
                };
                let elem = self
                    .var_types
                    .vec_elem_types
                    .get(v.as_str())
                    .or_else(|| self.var_types.slice_elem_types.get(v.as_str()))?;
                match elem {
                    BasicTypeEnum::StructType(t) => Some(*t),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// The element `TypeExpr`s of a tuple-typed place expression (`<struct>.f`
    /// where `f` is declared a tuple, or a tuple-index thereof). `&self`.
    pub(super) fn place_chain_tuple_tes(&self, expr: &Expr) -> Option<Vec<TypeExpr>> {
        match &expr.kind {
            // A bare tuple-typed local — `let e: (String, i64) = …`, a Map/Set
            // `.entries()` loop binding, etc. Recover its element TEs from the
            // per-variable registry populated at the binding site. Without this
            // arm, a method on a tuple element of a plain tuple variable
            // (`e.0.bytes()`) had no tuple-TE source, so the tuple-index
            // receiver path bailed and `.bytes()` fell through to "no handler
            // for method on non-identifier receiver" (B-2026-07-09-1, the Map
            // Message case).
            ExprKind::Identifier(name) => self.tuple_var_elem_tes(name),
            ExprKind::FieldAccess { object, field } => {
                let obj_ty = self.place_chain_type_name(object)?;
                let idx = self
                    .type_decls
                    .struct_field_names
                    .get(obj_ty.as_str())?
                    .iter()
                    .position(|n| n == field)?;
                match &self
                    .type_decls
                    .struct_field_type_exprs
                    .get(obj_ty.as_str())?
                    .get(idx)?
                    .kind
                {
                    TypeKind::Tuple(elems) => Some(elems.clone()),
                    _ => None,
                }
            }
            ExprKind::TupleIndex { object, index } => {
                let outer = self.place_chain_tuple_tes(object)?;
                match &outer.get(*index as usize)?.kind {
                    TypeKind::Tuple(elems) => Some(elems.clone()),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Root identifier of a place chain (`h.pe.0` → `h`, `self.f` → `self`).
    /// `None` for a non-place root (call result, literal). Used to gate
    /// move-out suppression on whether the root is owned vs caller-retains.
    pub(super) fn place_root_ident(expr: &Expr) -> Option<&str> {
        match &expr.kind {
            ExprKind::Identifier(n) => Some(n.as_str()),
            ExprKind::SelfValue => Some("self"),
            ExprKind::FieldAccess { object, .. }
            | ExprKind::TupleIndex { object, .. }
            | ExprKind::Index { object, .. } => Self::place_root_ident(object),
            _ => None,
        }
    }

    /// #21 — `let x = <place>.N` moving a heap tuple ELEMENT out of an owned
    /// struct/tuple. Cap-zero that element in the SOURCE so the owning struct's
    /// `NestedTuple` drop no-ops on the buffer the new binding now owns — the
    /// tuple-index peer of `suppress_struct_field_move_into_literal` (#19's enum
    /// field move-out, which handles only a named `FieldAccess` source). Bails on
    /// a caller-retains (`owned_struct_params`) root — its deep-copy owns the
    /// buffer, there is no value drop to suppress — and on a non-place root.
    pub(super) fn suppress_tuple_index_move_source(&mut self, value: &Expr) {
        let ExprKind::TupleIndex { object, index } = &value.kind else {
            return;
        };
        match Self::place_root_ident(value) {
            Some(root) if self.borrow_vars.owned_struct_params.contains(root) => return,
            Some(_) => {}
            None => return,
        }
        // B-2026-08-13-19 — a `UseAfterMove` defensive copy was emitted at this
        // consume site, so the new binding owns its OWN buffers and nothing was
        // transferred. Zeroing here would empty a source that still owns
        // everything it had: `let r = t.0; …; let chk = t.0;` read `chk`'s Vec
        // field back as EMPTY on both compiled backends where the interpreter
        // read its contents.
        //
        // Keyed on `uam_copied_sites` — what was REALLY copied — not on
        // `uam_consume_sites`, for the reason that set exists: skipping the
        // zeroing at a site where no copy happened would leave two owners of one
        // buffer, turning a wrong read into a double free. The copy half is
        // `uam_defensive_copy_tuple_elem`, and the two are inseparable by
        // construction because both key on this one set.
        if self
            .span_tables
            .uam_copied_sites
            .contains(&(value.span.offset, value.span.length))
        {
            return;
        }
        let Some(elems) = self.place_chain_tuple_tes(object) else {
            return;
        };
        let Some(mut te) = elems.get(*index as usize).cloned() else {
            return;
        };
        // B-2026-08-03-3 — `place_chain_tuple_tes` reconstructs elements from
        // recorded type NAMES, and an element with no recorded name renders as
        // an EMPTY path, which every arm below reads as a no-drop leaf. That is
        // how an `Option[Res]` element silently skipped its move-out
        // neutralization. Patch ONLY that degenerate case from the let-site's
        // own record; the names-derived spelling is left alone everywhere else,
        // because it is the one the Vec/String arms were tuned against (an
        // f-string element's inferred TE spells `str`, and preferring it
        // wholesale regressed `asan_tuple_index_assignment_heap_elems_freed`).
        let te_is_empty = matches!(&te.kind, TypeKind::Path(p) if p.segments.is_empty());
        if te_is_empty {
            if let ExprKind::Identifier(src) = &object.kind {
                if let Some(recorded) = self
                    .var_types
                    .tuple_var_elem_tes
                    .get(src.as_str())
                    .and_then(|tes| tes.get(*index as usize).cloned())
                {
                    te = recorded;
                }
            }
        }
        let Some(base_ptr) = self.field_chain_place_ptr(object) else {
            return;
        };
        let Some(tuple_ty) = self.place_chain_aggregate_llvm_type(object) else {
            return;
        };
        self.zero_tuple_elem_cap_at(base_ptr, tuple_ty, *index as u32, &te);
        // B-2026-08-03-3 — the BODIES half of the same move-out. Cap-zeroing
        // neutralizes the source's MEMORY drop but says nothing about its
        // `__karac_dropelems_tuple_*` walk, which kept firing element `index`'s
        // user Drop body over the just-zeroed slot (`drop 1 ` with an empty
        // name, while the interpreter printed a full duplicate). Re-register
        // the walk with that element masked out; the new binding is the sole
        // owner of its body.
        if let ExprKind::Identifier(src) = &object.kind {
            let src = src.clone();
            self.disarm_tuple_elem_bodies_at(&src, *index as u32, tuple_ty);
        }
    }

    /// Tuple-element sibling of [`Self::suppress_inline_result_payload_cleanup`]
    /// / `suppress_inline_option_payload_cleanup`: a `match t.N { Some(x) => ..
    /// }` / `{ Ok(v) => .. }` arm that binds the payload OUT, where element `N`
    /// is an `Option`/`Result` whose payload the tuple's own drop now frees
    /// (B-2026-08-03-3). Those two suppressors key on a BINDING registered in
    /// `inline_{option,result}_payload_vars`; a tuple element has no binding, so
    /// the consuming arm and the tuple drop both owned the buffer.
    ///
    /// Emits into the ARM BODY block, so the zeroing is runtime-conditional on
    /// that arm being taken — a non-consuming or wildcard arm leaves the tuple
    /// the sole owner. The bodies retraction beside it is STATIC, matching the
    /// documented trade at `suppress_container_elem_bodies_for_var`: an
    /// over-suppressed body prints once too few (a leak, the safe side), an
    /// under-suppressed one prints twice.
    pub(super) fn suppress_tuple_elem_optres_payload_cleanup(
        &mut self,
        scrutinee: &Expr,
        pattern: &Pattern,
    ) {
        let ExprKind::TupleIndex { object, index } = &scrutinee.kind else {
            return;
        };
        let ExprKind::Identifier(src) = &object.kind else {
            return;
        };
        let PatternKind::TupleVariant { path, patterns } = &pattern.kind else {
            return;
        };
        if !matches!(
            path.last().map(|s| s.as_str()),
            Some("Some") | Some("Ok") | Some("Err")
        ) {
            return;
        }
        if !patterns.iter().any(pattern_consumes_field) {
            return;
        }
        let src = src.clone();
        let Some(te) = self
            .var_types
            .tuple_var_elem_tes
            .get(src.as_str())
            .cloned()
            .or_else(|| self.tuple_var_elem_tes(src.as_str()))
            .and_then(|tes| tes.get(*index as usize).cloned())
        else {
            return;
        };
        if !self.tuple_elem_optres_drop_ok(&te) {
            return;
        }
        let Some(slot) = self.variables.get(src.as_str()).copied() else {
            return;
        };
        let inkwell::types::BasicTypeEnum::StructType(agg_ty) = slot.ty else {
            return;
        };
        self.zero_tuple_elem_cap_at(slot.ptr, agg_ty, *index as u32, &te);
        self.disarm_tuple_elem_bodies_at(&src, *index as u32, agg_ty);
    }

    /// B-2026-08-03-8 — the BODIES half of a struct-field move-out
    /// (`let x = h.o`), applied at the same three let-site positions that call
    /// `suppress_struct_field_move_into_literal` (its MEMORY half). Self-gated
    /// exactly like that suppressor — an Identifier / `self` object, a
    /// non-`owned_struct_params` root — so it fires in the same cases and no
    /// others. Idempotent: the mask accumulates, so being called from more than
    /// one of those positions for the same field is harmless.
    pub(super) fn disarm_struct_field_move_bodies(&mut self, value: &Expr) {
        let ExprKind::FieldAccess { object, field } = &value.kind else {
            return;
        };
        // B-2026-08-13-14 — the bodies half of the same disarm skip. See
        // `suppress_struct_field_move_into_literal`: at a site where
        // `uam_defensive_copy_field` produced an independent buffer the source
        // is still the owner of its own and its cleanup must stay armed.
        if self
            .span_tables
            .uam_copied_sites
            .contains(&(value.span.offset, value.span.length))
        {
            return;
        }
        let obj = match &object.kind {
            ExprKind::Identifier(o) => o.clone(),
            ExprKind::SelfValue => "self".to_string(),
            _ => return,
        };
        if self.borrow_vars.owned_struct_params.contains(obj.as_str()) {
            return;
        }
        let Some(fidx) = self
            .var_types
            .var_type_names
            .get(obj.as_str())
            .and_then(|sn| self.type_decls.struct_field_names.get(sn.as_str()))
            .and_then(|names| names.iter().position(|n| n == field))
        else {
            return;
        };
        self.disarm_struct_field_bodies_at(&obj, fidx);
    }

    /// Struct-FIELD sibling of [`Self::disarm_tuple_elem_bodies_at`]: mask field
    /// `field_idx` out of `var_name`'s `__karac_dropbodies_*` walk after a
    /// move-out, keeping every OTHER field's body armed (B-2026-08-03-8).
    /// Retracts through `suppress_struct_field_bodies_for_var` — the CONTAINER
    /// retraction does not match this action family, which is what made the
    /// first attempt add an action rather than replace one. The struct's own
    /// `impl Drop` body wrapper is a separate action and stays armed.
    pub(super) fn disarm_struct_field_bodies_at(&mut self, var_name: &str, field_idx: usize) {
        let Some(struct_name) = self.var_types.var_type_names.get(var_name).cloned() else {
            return;
        };
        let Some(slot) = self.variables.get(var_name).copied() else {
            return;
        };
        let subst = self
            .type_decls
            .enum_inst_var_types
            .get(var_name)
            .cloned()
            .map(|i| self.generic_struct_subst_from_inst(&struct_name, &i))
            .unwrap_or_default();
        let skip = self
            .type_decls
            .struct_moved_field_bodies
            .entry(var_name.to_string())
            .or_default();
        skip.insert(field_idx);
        // B-2026-08-28-23 — this site masks whole TOP-LEVEL fields (a field
        // moved out of the binding), so its tree carries `here` and no nested
        // level; the tree shape exists for the caller-side argument sites,
        // which resolve a path.
        let here_now: std::collections::BTreeSet<usize> = skip.iter().copied().collect();
        let skip = self.field_skip_tree_for_var(var_name, here_now);
        // B-2026-09-01-40 — a struct with its OWN `impl Drop` has NO per-binding
        // bodies walker to replace: it runs its field bodies from inside
        // `karac_drop_<T>`, the type-level wrapper, which is registered as a
        // separate `OwnWrapper` action. So the retraction below finds nothing
        // and the registration ADDS a second walk on top of the wrapper's,
        // running every surviving field's body twice.
        //
        // `let h = H { r: R { .. }, n: 4 }; let q = h.n;` measured
        // `dR3 dH dR3` on all three compiled surfaces against the
        // interpreter's `dH dR3`, with the field fully LIVE in both bodies --
        // so a body that releases a resource released it twice over a live
        // object. Nothing is moved here and the read is of a SCALAR, which is
        // why no memory error appears and ASAN stays green on it.
        //
        // The wrapper mask is the surgery that fits this shape, and is what the
        // param-view caller in `runtime.rs` already selects for an own-`Drop`
        // struct before ever reaching this helper. Doing it here too makes the
        // invariant structural rather than each caller's to remember.
        let owns_body = self
            .program_snapshot
            .as_deref()
            .is_some_and(|p| p.drop_method_keys.contains_key(&struct_name));
        if owns_body {
            self.disarm_user_drop_field_bodies_masked(slot.ptr, &struct_name, &skip);
            return;
        }
        self.suppress_struct_field_bodies_for_var(var_name);
        if let Some(bodies) =
            self.emit_user_drop_field_bodies_fn_skipping(&struct_name, &subst, &skip)
        {
            self.track_user_drop_var_with_fn(
                &struct_name,
                var_name,
                slot.ptr,
                bodies,
                UserDropKind::StructFieldBodies,
            );
        }
    }

    /// B-2026-08-31-26 — the MATCH-ARM sibling of
    /// [`Self::disarm_struct_field_bodies_at`]. A consuming arm that
    /// destructures a bare struct scrutinee (`match h { H { r, .. } => … }`)
    /// moves each bound field into the arm's binding, and
    /// [`Self::suppress_destructured_struct_pattern_cleanup`] already zeroes
    /// those fields' MEMORY in the source so nothing is freed twice. The
    /// BODIES half was missing: the source's `__karac_dropbodies_*` walk still
    /// visited the moved-out field, so a field type with a user `impl Drop`
    /// ran its body a SECOND time, on the husk the cap-zeroing had just left
    /// behind — `dR47 dR0` where `dR47` is due, `dR0` reading a cleared object.
    ///
    /// No memory error, which is why it stayed quiet: the buffer is freed
    /// exactly once and ASAN sees nothing. Only the observable body count is
    /// wrong, so a fixture has to assert the COUNT, not just that the program
    /// survives.
    ///
    /// Masks in place through [`Self::replace_user_drop_fn_for_var`] rather
    /// than re-registering, for the reason spelled out on the payload sibling
    /// below: this runs inside a match ARM whose cleanup frame is inner to the
    /// owner's, and a re-register would move the owner's walk into the arm and
    /// drain it there.
    pub(super) fn disarm_arm_destructured_struct_field_bodies(
        &mut self,
        scrutinee: &Expr,
        pattern: &Pattern,
    ) {
        // A borrow-mode arm binds a VIEW: the source keeps ownership and its
        // walk must stay armed. Same gate the Option/Result field sibling
        // above uses, and the invariant the read-only-arm family
        // (B-2026-08-30-47 / -52) rests on.
        if self.pattern_state.pattern_binding_is_borrow {
            return;
        }
        let PatternKind::Struct { fields, .. } = &pattern.kind else {
            return;
        };
        let var_name = match &scrutinee.kind {
            ExprKind::Identifier(n) => n.clone(),
            ExprKind::SelfValue => "self".to_string(),
            _ => return,
        };
        let Some(struct_name) = self
            .var_types
            .var_type_names
            .get(var_name.as_str())
            .cloned()
        else {
            return;
        };
        // A `shared` struct drops through refcounts, not this walk.
        if self.type_decls.shared_types.contains_key(&struct_name) {
            return;
        }
        let Some(field_names) = self
            .type_decls
            .struct_field_names
            .get(struct_name.as_str())
            .cloned()
        else {
            return;
        };
        let field_type_names = self
            .type_decls
            .struct_field_type_names
            .get(struct_name.as_str())
            .cloned()
            .unwrap_or_default();
        let mut masked_any = false;
        for field_pat in fields {
            let Some(idx) = field_names.iter().position(|n| n == &field_pat.name) else {
                continue;
            };
            // A WHOLE-field move into one binding masks the field outright.
            let whole_move = match &field_pat.pattern {
                None => true,
                Some(p) => matches!(p.kind, PatternKind::Binding(_)),
            };
            if whole_move {
                self.type_decls
                    .struct_moved_field_bodies
                    .entry(var_name.clone())
                    .or_default()
                    .insert(idx);
                masked_any = true;
                continue;
            }
            // B-2026-08-31-30 — a NESTED struct sub-pattern moved a field of
            // the INNER struct out. The outer field did NOT move, so masking it
            // outright would lose every other body it owes; record the inner
            // indices instead and let `FieldSkipTree::nested` mask inside that
            // field's own walker. The memory twin in
            // `suppress_destructured_struct_pattern_cleanup_at` descends by
            // recursion at the same point.
            let Some(sub) = &field_pat.pattern else {
                continue;
            };
            let PatternKind::Struct {
                fields: inner_fields,
                ..
            } = &sub.kind
            else {
                continue;
            };
            let inner_fields = inner_fields.clone();
            let Some(inner_name) = field_type_names.get(idx).and_then(|o| o.clone()) else {
                continue;
            };
            let Some(inner_field_names) = self
                .type_decls
                .struct_field_names
                .get(inner_name.as_str())
                .cloned()
            else {
                continue;
            };
            for ifp in &inner_fields {
                let inner_whole = match &ifp.pattern {
                    None => true,
                    Some(p) => matches!(p.kind, PatternKind::Binding(_)),
                };
                if !inner_whole {
                    continue;
                }
                if let Some(iidx) = inner_field_names.iter().position(|n| n == &ifp.name) {
                    self.type_decls
                        .struct_moved_nested_field_bodies
                        .entry(var_name.clone())
                        .or_default()
                        .entry(idx)
                        .or_default()
                        .insert(iidx);
                    masked_any = true;
                }
            }
        }
        if !masked_any {
            return;
        }
        if !self.variables.contains_key(var_name.as_str()) {
            return;
        }
        let subst = self
            .type_decls
            .enum_inst_var_types
            .get(var_name.as_str())
            .cloned()
            .map(|i| self.generic_struct_subst_from_inst(&struct_name, &i))
            .unwrap_or_default();
        // Both masks re-read from their maps so repeated arms accumulate and
        // the whole-field and payload-only kinds compose.
        let here: std::collections::BTreeSet<usize> = self
            .type_decls
            .struct_moved_field_bodies
            .get(var_name.as_str())
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        let skip = self.field_skip_tree_for_var(var_name.as_str(), here);
        match self.emit_user_drop_field_bodies_fn_skipping(&struct_name, &subst, &skip) {
            Some(bodies) => {
                // As on the payload sibling: no fallback registration when
                // there is no action to replace. An absent action means the
                // owner's walk lives elsewhere (a `ref self` receiver, say),
                // and inventing one here fires the caller's struct inside the
                // callee.
                let _ = self.replace_user_drop_fn_for_var(
                    &var_name,
                    UserDropKind::StructFieldBodies,
                    bodies,
                );
            }
            // Nothing survives the mask — leave the action retracted.
            None => self.suppress_struct_field_bodies_for_var(&var_name),
        }
    }

    /// B-2026-08-29-33 — the PAYLOAD-ONLY sibling of
    /// [`Self::disarm_struct_field_bodies_at`]: a consuming arm over
    /// `<var>.<field>` (`match s.e { E.A(r) => { let m = r; … } }`) took the
    /// enum payload, so the owner's walk must stop running ITS body — while
    /// still running the enum field's OWN `impl Drop` body, because the enum
    /// object did not move, only its payload did.
    ///
    /// Masking the whole field through the sibling above would lose that own
    /// body (`dE`); not masking at all ran the payload's body TWICE, the second
    /// time on the slot `suppress_destructured_struct_field_enum_cleanup` had
    /// just cap-zeroed — `m8 dR8 dE dR0` where `m8 dR8 dE` is due, `dR0` being a
    /// user body reading its own fields off a zeroed object. The identifier
    /// spelling gets this for free by having two separate cleanup actions; a
    /// field has one walk, so the split has to happen inside it.
    pub(super) fn disarm_struct_field_enum_payload_bodies_at(
        &mut self,
        var_name: &str,
        field_path: &[usize],
    ) {
        let Some(struct_name) = self.var_types.var_type_names.get(var_name).cloned() else {
            return;
        };
        // Presence check only — the slot pointer is no longer needed, since the
        // masked walker replaces an EXISTING action in place rather than
        // registering a new one against the variable's slot.
        if !self.variables.contains_key(var_name) {
            return;
        }
        let subst = self
            .type_decls
            .enum_inst_var_types
            .get(var_name)
            .cloned()
            .map(|i| self.generic_struct_subst_from_inst(&struct_name, &i))
            .unwrap_or_default();
        self.type_decls
            .struct_moved_field_payload_bodies
            .entry(var_name.to_string())
            .or_default()
            .insert(field_path.to_vec());
        // Both masks are re-read from their maps so repeated arms accumulate
        // and the two kinds compose — a struct can have one field moved
        // wholesale and another's payload taken.
        let here: std::collections::BTreeSet<usize> = self
            .type_decls
            .struct_moved_field_bodies
            .get(var_name)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        let skip = self.field_skip_tree_for_var(var_name, here);
        // IN-PLACE swap, not retract-and-re-register: this runs inside a match
        // ARM, whose cleanup frame is inner to the owner's, so a re-register
        // would move the owner's walk into the arm and drain it there. See
        // `replace_user_drop_fn_for_var`.
        match self.emit_user_drop_field_bodies_fn_skipping(&struct_name, &subst, &skip) {
            Some(bodies) => {
                // NO fallback registration when nothing matched. An absent
                // action means the owner has no bodies walk here — a `ref self`
                // receiver, say, whose CALLER owns the walk — and inventing one
                // makes the caller's struct fire inside the callee: measured
                // `dR8 dE n8 dE dR8`, `dE` twice, for a `ref self` method
                // matching `self.e` and moving the payload out. Nothing to hand
                // over is not the same as something to add.
                let _ = self.replace_user_drop_fn_for_var(
                    var_name,
                    UserDropKind::StructFieldBodies,
                    bodies,
                );
            }
            // Nothing survives the mask — leave the action retracted, the same
            // reading the whole-field sibling gives a `None`.
            None => self.suppress_struct_field_bodies_for_var(var_name),
        }
    }

    /// Mask tuple element `index` out of `var_name`'s element-bodies walk after
    /// a move-out (`let x = t.N`), keeping every OTHER element's body armed.
    /// The whole-var [`Self::suppress_container_elem_bodies_for_var`] is too
    /// coarse here — `let t = (r0, r1); let x = t.0;` must still run `r1`'s
    /// body. Masks accumulate across move-outs; when nothing survives, the
    /// action simply stays retracted (B-2026-08-03-3).
    pub(super) fn disarm_tuple_elem_bodies_at(
        &mut self,
        var_name: &str,
        index: u32,
        tuple_ty: inkwell::types::StructType<'ctx>,
    ) {
        let Some(elem_tes) = self.var_types.tuple_var_elem_tes.get(var_name).cloned() else {
            return;
        };
        let Some(slot) = self.variables.get(var_name).copied() else {
            return;
        };
        let skip = self
            .tuple_moved_elem_bodies
            .entry(var_name.to_string())
            .or_default();
        skip.insert(index);
        let skip = skip.clone();
        self.suppress_container_elem_bodies_for_var(var_name);
        if let Some(bodies) =
            self.emit_tuple_elem_user_drop_bodies_fn_skipping(tuple_ty, &elem_tes, &skip)
        {
            self.track_user_drop_var_with_fn(
                "",
                var_name,
                slot.ptr,
                bodies,
                UserDropKind::ContainerElemBodies,
            );
        }
    }

    /// B-2026-08-29-33 — the PAYLOAD-ONLY sibling of
    /// [`Self::disarm_tuple_elem_bodies_at`], and the tuple peer of
    /// [`Self::disarm_struct_field_enum_payload_bodies_at`]. A consuming arm
    /// over `<var>.<index>` took the enum element's payload, so the tuple's
    /// element walk must stop running ITS body while still running the enum's
    /// own. Masking the whole element loses `dE`; masking nothing runs the
    /// payload's body twice, the second time on the cap-zeroed slot.
    pub(super) fn disarm_tuple_elem_enum_payload_bodies_at(
        &mut self,
        var_name: &str,
        index: u32,
        tuple_ty: inkwell::types::StructType<'ctx>,
    ) {
        let Some(elem_tes) = self.var_types.tuple_var_elem_tes.get(var_name).cloned() else {
            return;
        };
        if !self.variables.contains_key(var_name) {
            return;
        }
        self.tuple_moved_elem_payload_bodies
            .entry(var_name.to_string())
            .or_default()
            .insert(index);
        let skip = self
            .tuple_moved_elem_bodies
            .get(var_name)
            .cloned()
            .unwrap_or_default();
        let payload_skip = self
            .tuple_moved_elem_payload_bodies
            .get(var_name)
            .cloned()
            .unwrap_or_default();
        // In-place swap for the same reason as the struct sibling above.
        match self.emit_tuple_elem_user_drop_bodies_fn_masked(
            tuple_ty,
            &elem_tes,
            &skip,
            &payload_skip,
        ) {
            Some(bodies) => {
                // No fallback registration — see the struct sibling above.
                let _ = self.replace_user_drop_fn_for_var(
                    var_name,
                    UserDropKind::ContainerElemBodies,
                    bodies,
                );
            }
            None => self.suppress_container_elem_bodies_for_var(var_name),
        }
    }

    /// B-2026-08-29-33 — the BODIES half of
    /// [`Self::suppress_destructured_struct_field_enum_cleanup`], which until
    /// now did only the MEMORY half (cap-zeroing the source field). A consuming
    /// arm over a projection place took the payload, so whoever owns the place
    /// must stop running its `Drop` body — otherwise it fires twice, the second
    /// time reading its own fields off the slot the cap-zeroing just emptied.
    ///
    /// ONE HOP only, deliberately: `<ident>.<field>` and `<ident>.<index>` are
    /// the shapes whose owner has a walk this can re-register. A deeper chain
    /// (`w.s.e`) would need the mask threaded through `FieldSkipTree::nested`
    /// against the intermediate field's walker, and it keeps today's behaviour
    /// until something measures it.
    /// B-2026-08-29-44 — carry a binding's move-out masks across a WHOLE-VALUE
    /// REBIND (`let s2 = s;`).
    ///
    /// Every mask in this family is keyed on the BINDING, and a rebind
    /// registers the destination afresh — so the walk the source had withheld
    /// came back, and the param view's body ran a second time. The ALL-VIEWS
    /// case has no such hole because it marks the binding a param view and
    /// view-ness already propagates (B-2026-08-01-15); only the MIXED case,
    /// whose binding is not a view, loses its mask.
    ///
    /// Copies rather than moves: the source binding is still live for its own
    /// scope-exit walk, which must stay masked too.
    pub(super) fn transfer_move_masks_on_rebind(&mut self, src: &str, dst: &str) {
        if src == dst {
            return;
        }
        if let Some(v) = self.type_decls.struct_moved_field_bodies.get(src).cloned() {
            self.type_decls
                .struct_moved_field_bodies
                .entry(dst.to_string())
                .or_default()
                .extend(v);
        }
        if let Some(v) = self
            .type_decls
            .struct_moved_field_payload_bodies
            .get(src)
            .cloned()
        {
            self.type_decls
                .struct_moved_field_payload_bodies
                .entry(dst.to_string())
                .or_default()
                .extend(v);
        }
        if let Some(v) = self
            .type_decls
            .struct_moved_nested_field_bodies
            .get(src)
            .cloned()
        {
            let e = self
                .type_decls
                .struct_moved_nested_field_bodies
                .entry(dst.to_string())
                .or_default();
            for (outer, inner) in v {
                e.entry(outer).or_default().extend(inner);
            }
        }
        if let Some(v) = self.tuple_moved_elem_bodies.get(src).cloned() {
            self.tuple_moved_elem_bodies
                .entry(dst.to_string())
                .or_default()
                .extend(v);
        }
    }

    /// The COMPLETE field-skip tree for `var_name`, given this level's
    /// whole-field mask.
    ///
    /// Three rows now feed one tree and two of them feed `nested` from
    /// opposite directions: B-2026-08-29-36's deep projection PATHS mask a
    /// payload one or more levels down (`match w.s.e`), and B-2026-08-31-30's
    /// nested struct SUB-PATTERNS mask whole inner fields
    /// (`match q { Q { h: H { r, .. } } => … }`). A variable can carry both,
    /// and every site that rebuilds its masked walker replaces the action
    /// wholesale — so a site that assembles only its own row's half silently
    /// drops the other's, depending on which arm compiled last. One builder,
    /// used by all three sites, is what makes that unrepresentable.
    pub(super) fn field_skip_tree_for_var(
        &self,
        var_name: &str,
        here: std::collections::BTreeSet<usize>,
    ) -> super::synth_drop::FieldSkipTree {
        let payload_paths = self
            .type_decls
            .struct_moved_field_payload_bodies
            .get(var_name)
            .cloned()
            .unwrap_or_default();
        let mut skip = super::synth_drop::FieldSkipTree::from_payload_paths(here, &payload_paths);
        if let Some(m) = self
            .type_decls
            .struct_moved_nested_field_bodies
            .get(var_name)
        {
            for (outer, inner) in m {
                skip.nested
                    .entry(*outer)
                    .or_default()
                    .here
                    .extend(inner.iter().copied());
            }
        }
        skip
    }

    /// B-2026-08-29-36 — resolve a chain of struct FIELD accesses rooted at a
    /// plain identifier into `(root, [field index per hop])`.
    ///
    /// One hop is the shape B-2026-08-29-33 handled; anything deeper had no
    /// resolution at all, which is why `match w.s.e { .. }` kept the unmasked
    /// walk. `None` for any root that is not an identifier, and for any hop
    /// whose owning type or field name cannot be resolved — the same
    /// uncertain-⇒-silent rule the one-hop form applied.
    fn projection_field_index_path(&self, expr: &Expr) -> Option<(String, Vec<usize>)> {
        let ExprKind::FieldAccess { object, field } = &expr.kind else {
            return None;
        };
        let (root, mut path) = match &object.kind {
            ExprKind::Identifier(root) => (root.clone(), Vec::new()),
            ExprKind::FieldAccess { .. } => self.projection_field_index_path(object)?,
            _ => return None,
        };
        // The owning type of THIS hop: the root's recorded type at depth 1,
        // and the resolved chain type deeper in.
        let obj_ty = if path.is_empty() {
            self.var_types.var_type_names.get(root.as_str())?.clone()
        } else {
            self.place_chain_type_name(object)?
        };
        let idx = self
            .type_decls
            .struct_field_names
            .get(obj_ty.as_str())?
            .iter()
            .position(|n| n == field)?;
        path.push(idx);
        Some((root, path))
    }

    fn disarm_projection_enum_payload_bodies(&mut self, scrutinee: &Expr) {
        match &scrutinee.kind {
            ExprKind::FieldAccess { .. } => {
                let Some((root, path)) = self.projection_field_index_path(scrutinee) else {
                    return;
                };
                if path.is_empty() {
                    return;
                }
                self.disarm_struct_field_enum_payload_bodies_at(&root, &path);
            }
            ExprKind::TupleIndex { object, index } => {
                let ExprKind::Identifier(root) = &object.kind else {
                    return;
                };
                let root = root.clone();
                let index = *index as u32;
                let Some(elem_tes) = self
                    .var_types
                    .tuple_var_elem_tes
                    .get(root.as_str())
                    .cloned()
                else {
                    return;
                };
                let tes: Vec<TypeExpr> = elem_tes.clone();
                let tuple_te = TypeExpr {
                    kind: TypeKind::Tuple(tes),
                    span: scrutinee.span,
                };
                let inkwell::types::BasicTypeEnum::StructType(agg) =
                    self.llvm_type_for_type_expr(&tuple_te)
                else {
                    return;
                };
                self.disarm_tuple_elem_enum_payload_bodies_at(&root, index, agg);
            }
            _ => {}
        }
    }

    /// Type name of a place-expression root for [`Self::field_chain_place_ptr`]'s
    /// field GEP and the #18 leaf-enum lookup. Identical to
    /// [`Self::type_name_of_expr`] except it also resolves a `vec[i]` index to the
    /// Vec's element type, recursing through it for a `vec[i].f` chain. The shared
    /// resolver deliberately returns `None` for `Index` (12 callers rely on that),
    /// so this generalization stays local to the match-suppression path.
    pub(super) fn place_chain_type_name(&self, expr: &Expr) -> Option<String> {
        match &expr.kind {
            ExprKind::Index { object, .. } => {
                if let ExprKind::Identifier(v) = &object.kind {
                    return match self
                        .var_types
                        .var_elem_type_exprs
                        .get(v.as_str())
                        .map(|te| &te.kind)
                    {
                        Some(TypeKind::Path(p)) => p.segments.last().cloned(),
                        _ => None,
                    };
                }
                // #38 — a FieldAccess/`self`-rooted Vec index (`self.tokens[i]`):
                // element type name from the collection FIELD's `Vec[E]`
                // TypeExpr. Pure resolution (no IR), so safe regardless of the
                // `ref self` element-pointer subtlety that gates `field_chain_place_ptr`.
                if let ExprKind::FieldAccess {
                    object: inner,
                    field,
                } = &object.kind
                {
                    let obj_ty = self.place_chain_type_name(inner)?;
                    let fidx = self
                        .type_decls
                        .struct_field_names
                        .get(obj_ty.as_str())?
                        .iter()
                        .position(|n| n == field)?;
                    let field_te = self
                        .type_decls
                        .struct_field_type_exprs
                        .get(obj_ty.as_str())?
                        .get(fidx)?;
                    let elem_te = vec_inner_type_expr(field_te)?;
                    if let TypeKind::Path(p) = &elem_te.kind {
                        return p.segments.last().cloned();
                    }
                }
                None
            }
            ExprKind::FieldAccess { object, field } => {
                let obj_ty = self.place_chain_type_name(object)?;
                let idx = self
                    .type_decls
                    .struct_field_names
                    .get(obj_ty.as_str())?
                    .iter()
                    .position(|n| n == field)?;
                self.type_decls
                    .struct_field_type_names
                    .get(obj_ty.as_str())?
                    .get(idx)?
                    .clone()
            }
            // #21 — `<struct>.tuplefield.N`: the element's declared type name
            // (e.g. `h.pe.0` → the enum `Tok`). Tuples carry no field-name table,
            // so resolve via the element `TypeExpr`s; for a tuple VAR / param root
            // (`p.0`), fall back to the recorded per-element type names.
            ExprKind::TupleIndex { object, index } => {
                if let Some(elems) = self.place_chain_tuple_tes(object) {
                    if let Some(TypeKind::Path(p)) = elems.get(*index as usize).map(|t| &t.kind) {
                        return p.segments.last().cloned();
                    }
                }
                if let ExprKind::Identifier(v) = &object.kind {
                    return self
                        .var_types
                        .tuple_var_elem_type_names
                        .get(v.as_str())?
                        .get(*index as usize)?
                        .clone();
                }
                None
            }
            _ => self.type_name_of_expr(expr),
        }
    }

    /// Core of [`Self::suppress_destructured_enum_payload_cleanup`], keyed on
    /// the source enum's alloca + name directly rather than resolving them
    /// from an identifier scrutinee. The B-track fresh-temp path
    /// (`materialize_freshtemp_enum_scrutinee`) has no identifier to resolve —
    /// it minted its own alloca for the temporary — so it calls this directly
    /// with that alloca. For every payload position the arm's pattern *moves*
    /// into a binding (`pattern_consumes_field`), zero the cap word in the
    /// source so the enum's `__karac_drop_<E>` walk skips it (the binding's own
    /// cleanup frees that buffer); unbound heap fields keep their cap and are
    /// freed by the drop walk.
    ///
    /// The consumed-position computation is shared with
    /// [`Self::enum_pattern_consumed_positions`], which the B-2026-07-30-11
    /// payload-BODIES disarm also consults so the two channels agree on what
    /// "this arm moved the payload out" means.
    pub(super) fn suppress_destructured_enum_payload_cleanup_at(
        &self,
        slot_ptr: PointerValue<'ctx>,
        enum_name: &str,
        pattern: &Pattern,
    ) {
        let layout = match self.type_decls.enum_layouts.get(enum_name) {
            Some(l) => l.clone(),
            None => return,
        };
        if layout.is_shared {
            return;
        }
        let Some((variant_name, consumed_positions)) =
            self.enum_pattern_consumed_positions(enum_name, pattern)
        else {
            return;
        };
        let drop_kinds = match layout.field_drop_kinds.get(&variant_name) {
            Some(k) => k,
            None => return,
        };
        let offsets = match layout.field_word_offsets.get(&variant_name) {
            Some(o) => o,
            None => return,
        };
        let i64_t = self.context.i64_type();
        let zero = i64_t.const_int(0, false);
        for &pos in &consumed_positions {
            let kind = match drop_kinds.get(pos) {
                Some(k) => k,
                None => continue,
            };
            let (start_word, num_words) = match offsets.get(pos) {
                Some(o) => *o,
                None => continue,
            };
            // `consumed_positions` already filtered to the fields this pattern
            // *moves* into a binding (a `Wildcard`/literal sub-pattern doesn't
            // claim ownership, so its field's drop must still fire to free the
            // payload). Only heap-bearing kinds need their source drop skipped.
            if !kind.is_heap_bearing() {
                continue;
            }
            // Zero EVERY payload word of the moved-out field, not just the
            // Vec/String cap. For `VecOrString` the cap word (LLVM index
            // `start_word + num_words`) is what the drop's `cap > 0` guard
            // reads; zeroing data/len too is harmless. For a `NestedStruct`
            // payload (B-2026-06-13-13) there is no single cap — its drop fn
            // reads caps/tags at various inner offsets — so zero the whole
            // word region: every inner `cap > 0` guard then skips and the
            // tag-dispatch lands on an all-zero variant, making the nested
            // drop a no-op. The bound binding's own cleanup frees it once.
            for w in 0..num_words {
                let word_index = (start_word + 1 + w) as u32;
                if let Ok(word_ptr) = self.builder.build_struct_gep(
                    layout.llvm_type,
                    slot_ptr,
                    word_index,
                    "match.dest.suppress.wp",
                ) {
                    let _ = self.builder.build_store(word_ptr, zero);
                }
            }
        }
    }

    /// `(variant name, declared-position indices of the payload fields this
    /// pattern CONSUMES)` — the positions it moves into a binding. `None` for a
    /// non-variant pattern, or a struct-variant whose field names can't be
    /// resolved.
    ///
    /// Both variant shapes move payload fields into bindings, and the variant
    /// name is the path's last segment for either. Tuple-variant fields are
    /// positional; struct-variant fields are named, so each named field is
    /// mapped to its declared position via `enum_variant_struct_field_names` —
    /// the same field order `field_drop_kinds` / `field_word_offsets` and the
    /// struct-variant constructor are keyed on. A field is consumed when its
    /// sub-pattern binds (directly or via a nested destructure); a
    /// `Wildcard`/literal sub-pattern doesn't claim ownership, so the source's
    /// drop must still fire (suppressing it would leak). A struct-variant
    /// shorthand field (`{ value }`, `pattern: None`) is a direct binding and
    /// always consumes.
    pub(super) fn enum_pattern_consumed_positions(
        &self,
        enum_name: &str,
        pattern: &Pattern,
    ) -> Option<(String, Vec<usize>)> {
        let variant_name = match &pattern.kind {
            PatternKind::TupleVariant { path, .. } | PatternKind::Struct { path, .. } => {
                path.last()?.clone()
            }
            _ => return None,
        };
        let positions: Vec<usize> = match &pattern.kind {
            PatternKind::TupleVariant { patterns, .. } => patterns
                .iter()
                .enumerate()
                .filter(|(_, sub)| pattern_consumes_field(sub))
                .map(|(i, _)| i)
                .collect(),
            PatternKind::Struct { fields, .. } => {
                let field_names = self.enum_variant_struct_field_names(enum_name, &variant_name)?;
                fields
                    .iter()
                    .filter(|fp| fp.pattern.as_ref().is_none_or(pattern_consumes_field))
                    .filter_map(|fp| field_names.iter().position(|n| n == &fp.name))
                    .collect()
            }
            _ => return None,
        };
        Some((variant_name, positions))
    }

    /// B-2026-07-30-11 (enum leg) — does `pattern` move out a payload position
    /// whose type runs a user `impl Drop` body? The gate for retracting the
    /// source binding's `__karac_dropelems_enum_<E>` action: once the payload
    /// belongs to the arm's binding, running the source's body would either
    /// print twice or (after the memory-side cap-zeroing) read a wiped payload.
    ///
    /// Distinct from the memory side's `is_heap_bearing` test on purpose. A
    /// `Res { id: i64 }` payload has no heap and nothing to cap-zero, yet its
    /// body still has to be disarmed — the same gate mistake the tuple leg had
    /// to avoid at its registration site, seen from the suppression end.
    fn enum_pattern_consumes_user_drop_payload(&self, enum_name: &str, pattern: &Pattern) -> bool {
        let Some((variant_name, consumed)) =
            self.enum_pattern_consumed_positions(enum_name, pattern)
        else {
            return false;
        };
        let Some((_, _, tes)) = self
            .enum_variant_field_type_exprs(enum_name)
            .into_iter()
            .find(|(_, n, _)| *n == variant_name)
        else {
            return false;
        };
        consumed.into_iter().any(|pos| {
            tes.get(pos)
                .and_then(|te| match &te.kind {
                    TypeKind::Path(p) => p.segments.first().cloned(),
                    _ => None,
                })
                .is_some_and(|n| self.type_runs_user_drop(&n, &mut Vec::new()))
        })
    }

    /// Shared-enum analog of [`Self::suppress_destructured_enum_payload_cleanup_at`]
    /// (which bails on `layout.is_shared`). When a `match` arm over a SHARED-enum
    /// RC box (`box_ptr`) MOVES a `Vec`/`String` payload field out into a binding,
    /// zero that field's payload words IN THE BOX so the box's
    /// `__karac_rc_drop_<E>` (whose `Vec`/`String` arm in `emit_shared_enum_field_drop`
    /// frees `cap > 0`) skips the buffer the binding now owns. Without it the
    /// binding (or a value it is moved into — a returned `String`) frees the buffer
    /// AND the box's eventual rc-drop frees it again → double-free (the basic
    /// `match e { S(s) => s }` over `shared enum E { S(String) }`).
    ///
    /// Only `EnumDropKind::VecOrString` fields directly inline in the box payload
    /// are touched: a shared-handle payload is rc-managed (the arm rc-incs it on
    /// extraction, the box rc-decs it), and a `NestedStruct` payload (inline or
    /// heap-boxed) is freed by the box drop's struct recursion — neither aliases a
    /// buffer the binding owns. The heap-word index mirrors
    /// `emit_shared_enum_rc_drop_fn`'s `start_word + 2` (the `{rc, tag}` prefix).
    pub(super) fn suppress_shared_enum_payload_move_out(
        &self,
        box_ptr: PointerValue<'ctx>,
        enum_name: &str,
        pattern: &Pattern,
    ) {
        let layout = match self.type_decls.enum_layouts.get(enum_name) {
            Some(l) => l.clone(),
            None => return,
        };
        if !layout.is_shared {
            return;
        }
        let heap_type = match self.type_decls.shared_types.get(enum_name) {
            Some(i) => i.heap_type,
            None => return,
        };
        let variant_name = match &pattern.kind {
            PatternKind::TupleVariant { path, .. } | PatternKind::Struct { path, .. } => {
                match path.last() {
                    Some(n) => n.as_str(),
                    None => return,
                }
            }
            _ => return,
        };
        let drop_kinds = match layout.field_drop_kinds.get(variant_name) {
            Some(k) => k,
            None => return,
        };
        let offsets = match layout.field_word_offsets.get(variant_name) {
            Some(o) => o,
            None => return,
        };
        // Declared-position indices of the payload fields this pattern *consumes*
        // (moves into a binding) — same derivation as the non-shared suppressor.
        let consumed_positions: Vec<usize> = match &pattern.kind {
            PatternKind::TupleVariant { patterns, .. } => patterns
                .iter()
                .enumerate()
                .filter(|(_, sub)| pattern_consumes_field(sub))
                .map(|(i, _)| i)
                .collect(),
            PatternKind::Struct { fields, .. } => {
                let field_names =
                    match self.enum_variant_struct_field_names(enum_name, variant_name) {
                        Some(n) => n,
                        None => return,
                    };
                fields
                    .iter()
                    .filter(|fp| fp.pattern.as_ref().is_none_or(pattern_consumes_field))
                    .filter_map(|fp| field_names.iter().position(|n| n == &fp.name))
                    .collect()
            }
            _ => return,
        };
        let i64_t = self.context.i64_type();
        let zero = i64_t.const_int(0, false);
        for &pos in &consumed_positions {
            if !matches!(
                drop_kinds.get(pos),
                Some(super::state::EnumDropKind::VecOrString)
            ) {
                continue;
            }
            let (start_word, num_words) = match offsets.get(pos) {
                Some(o) => *o,
                None => continue,
            };
            for w in 0..num_words {
                let word_index = (start_word + 2 + w) as u32;
                if let Ok(word_ptr) = self.builder.build_struct_gep(
                    heap_type,
                    box_ptr,
                    word_index,
                    "match.sh.suppress.wp",
                ) {
                    let _ = self.builder.build_store(word_ptr, zero);
                }
            }
        }
        // Map / Set payload (`Full(Map[i64, u64])`): the enum RC drop frees the
        // handle UNCONDITIONALLY via `karac_map_free_with_drop_vec` (no `cap > 0`
        // guard, unlike Vec/String), and `Map`/`Set` are NOT classified in
        // `EnumDropKind` (freed by field TYPE in `emit_shared_enum_field_drop`),
        // so the loop above never suppresses them. When the arm moves such a
        // payload out, zero the handle word in the box: the runtime free no-ops on
        // a null handle, so the moved binding frees the map exactly once instead of
        // the box's rc-drop double-freeing it (B-2026-07-08-22).
        let field_tes: Vec<TypeExpr> = self
            .enum_variant_field_type_exprs(enum_name)
            .into_iter()
            .find(|(_, name, _)| name == variant_name)
            .map(|(_, _, tes)| tes)
            .unwrap_or_default();
        for &pos in &consumed_positions {
            let is_map_or_set = field_tes.get(pos).is_some_and(|te| {
                matches!(&te.kind, crate::ast::TypeKind::Path(p)
                if matches!(
                    p.segments.last().map(String::as_str),
                    Some("Map") | Some("HashMap") | Some("Set") | Some("HashSet")
                ))
            });
            if !is_map_or_set {
                continue;
            }
            let (start_word, _num_words) = match offsets.get(pos) {
                Some(o) => *o,
                None => continue,
            };
            // The handle is the field's first (only) payload word.
            let word_index = (start_word + 2) as u32;
            if let Ok(word_ptr) = self.builder.build_struct_gep(
                heap_type,
                box_ptr,
                word_index,
                "match.sh.suppress.map.wp",
            ) {
                let _ = self.builder.build_store(word_ptr, zero);
            }
        }
    }

    /// B-2026-06-10-6 companion to
    /// [`Self::suppress_destructured_enum_payload_cleanup`] for inline-heap
    /// `Option[String]` / `Option[Vec[_]]` scrutinees. The `Option` layout
    /// is type-erased, so it carries no static `VecOrString` field kind and
    /// the generic suppression above can't fire for it. When the scrutinee
    /// is an identifier whose binding registered a
    /// `FreeInlineOptionPayload` (tracked in `inline_option_payload_vars`)
    /// and the arm binds the `Some` payload out, zero the source `Option`'s
    /// `cap` word (option field index 3 = the `cap` of the `{ptr,len,cap}`
    /// payload at words w0/w1/w2) so the scope-exit free's `cap > 0` guard
    /// skips — the bound payload's own cleanup frees it exactly once. A
    /// `Some(_)` / `None` arm binds nothing, so the source free must still
    /// fire and no suppression happens.
    /// B-2026-08-12-15 — hand the box inside an inline STRUCT payload's field
    /// over to the arm that binds that struct out, by zeroing the box word in
    /// the SCRUTINEE's slot so its let-site `NestedBoxedEnumDrop` skips.
    ///
    /// `let r: Result[W, i64] = …; match r { Result.Ok(w) => … }` over
    /// `struct W { o: Option[Option[i64]] }` has two owners the moment the let
    /// site is armed: this action, and the arm binding's own `StructDrop` —
    /// `__karac_drop_struct_W` walks the `Option` field and frees the box. Both
    /// fire, and it aborts with a glibc `free(): double free detected in
    /// tcache 2` at `-O0`.
    ///
    /// The arm wins rather than the scrutinee, which is the direction the rest
    /// of this family already moves ownership in (`suppress_destructured_enum_
    /// payload_cleanup`, the inline `Option`/`Result` cap-zeroes above): a
    /// binding that outlives the arm needs the value, so the source disarms.
    ///
    /// GATED ON THE ARM ACTUALLY BINDING, via the same `pattern_consumes_field`
    /// the inline siblings use. `Result.Ok(_)` registers no `StructDrop`, so
    /// zeroing there would orphan the box — measured clean today precisely
    /// because the scrutinee's action stays armed for it.
    ///
    /// Zeroes the WORD, not the tag, for the reason
    /// `suppress_nested_boxed_drop_for_var` gives about this action's two-tag
    /// guard: `Result`'s `Ok` is tag 0, so a tag-zero leaves the outer guard
    /// passing. The word index comes off the queued action
    /// (`inner_tag_field + 1`) so it cannot drift from
    /// `struct_payload_boxed_field_variants`' offset arithmetic.
    pub(super) fn suppress_struct_field_boxed_payload_arm_bind(
        &self,
        scrutinee: &Expr,
        pattern: &Pattern,
    ) {
        if self.pattern_state.pattern_binding_is_borrow {
            return;
        }
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return;
        };
        if !self
            .payload_vars
            .struct_field_boxed_payload_vars
            .contains(name.as_str())
        {
            return;
        }
        let PatternKind::TupleVariant { path, patterns } = &pattern.kind else {
            return;
        };
        if !patterns.iter().any(pattern_consumes_field) {
            return;
        }
        let Some(variant) = path.last() else {
            return;
        };
        let Some(slot) = self.variables.get(name.as_str()).copied() else {
            return;
        };
        // The registration is per-VARIANT (a `Result` can arm both `Ok` and
        // `Err`), and this store lands in the arm's own block — so it must
        // disarm only the action whose variant this arm is, or an `Err` arm
        // would strand the `Ok` box.
        let arm_tag = ["Option", "Result"]
            .iter()
            .filter(|e| path.len() < 2 || path[path.len() - 2] == **e)
            .find_map(|e| {
                self.type_decls
                    .enum_layouts
                    .get(*e)?
                    .tags
                    .get(variant.as_str())
                    .copied()
            });
        let Some(arm_tag) = arm_tag else {
            return;
        };
        let i64_t = self.context.i64_type();
        for action in self.drop_rc.scope_cleanup_actions.iter().flatten() {
            if let crate::codegen::state::CleanupAction::NestedBoxedEnumDrop {
                enum_slot,
                enum_ty,
                outer_tag,
                inner_tag_field,
                ..
            } = action
            {
                if *enum_slot != slot.ptr || *outer_tag != arm_tag {
                    continue;
                }
                if let Ok(w0) = self.builder.build_struct_gep(
                    *enum_ty,
                    slot.ptr,
                    inner_tag_field + 1,
                    "nbox.armbind.w0",
                ) {
                    let _ = self.builder.build_store(w0, i64_t.const_zero());
                }
            }
        }
    }

    /// B-2026-09-01-30 — carry an owned by-value param's deep-copy obligation
    /// onto a whole-value PATTERN binding of it (`match` arm, `if let`,
    /// `while let`, `let ... else`).
    ///
    /// `owned_vecstr_params` names the params whose `String`/`Vec` header the
    /// callee received by value while the CALLER still frees the buffer, so
    /// every retaining consume site inside the callee must deep-copy rather
    /// than hand the same buffer on. The set holds param names, and a match arm
    /// rebinds the value under a new one — so `match b { v => { return v; } }`
    /// asked the return-position copy about `v`, got "not an owned param", and
    /// moved. Both sides then freed one buffer.
    ///
    /// Narrow on purpose. It fires only for a scrutinee that is a bare
    /// identifier naming such a param, and only for a pattern that binds the
    /// WHOLE value (`v =>`), never a destructure — a sub-pattern binds a PART,
    /// whose ownership is the business of the payload/field machinery beside
    /// this call and which has its own disarm rules. The elem-type side tables
    /// are mirrored too, because the copy helper reads the element type off the
    /// name it is given and silently no-ops without it.
    pub(super) fn propagate_owned_param_to_whole_arm_binding(
        &mut self,
        scrutinee: &Expr,
        pattern: &Pattern,
    ) {
        let ExprKind::Identifier(src) = &scrutinee.kind else {
            return;
        };
        if !self.borrow_vars.owned_vecstr_params.contains(src.as_str()) {
            return;
        }
        let PatternKind::Binding(name) = &pattern.kind else {
            return;
        };
        // A `Binding` whose name is a unit enum variant is a tag test, not a
        // catch-all binding — the same guard the string-dispatch analyzer uses.
        let variant = name.rsplit('.').next().unwrap_or(name);
        if self.enum_tag_for_variant(variant).is_some() {
            return;
        }
        let Some(elem_ty) = self.var_types.vec_elem_types.get(src.as_str()).copied() else {
            return;
        };
        let elem_te = self
            .var_types
            .var_elem_type_exprs
            .get(src.as_str())
            .cloned();
        self.var_types.vec_elem_types.insert(name.clone(), elem_ty);
        if let Some(te) = elem_te {
            self.var_types.var_elem_type_exprs.insert(name.clone(), te);
        }
        self.borrow_vars.owned_vecstr_params.insert(name.clone());
    }

    pub(super) fn suppress_inline_option_payload_cleanup(
        &self,
        scrutinee: &Expr,
        pattern: &Pattern,
    ) {
        // B-2026-08-08-25 — suppression TRANSFERS the payload to the arm
        // binding. When the classifier proved the arms only borrow it, that
        // binding registers no owner (the borrow path skips `track_vec_var`),
        // so there is nobody to transfer to: disarming the source here would
        // leave the buffer freed at arm exit and the still-live source reading
        // it afterward.
        if self
            .pattern_state
            .pattern_binding_source_retains_inline_payload
        {
            return;
        }
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return;
        };
        // B-2026-08-06-27 — a passthrough result binding owns nothing (its
        // source does), so a disarm aimed at it must land on the source.
        let name: &str = self
            .payload_vars
            .passthrough_owner_alias
            .get(name.as_str())
            .map_or(name.as_str(), |s| s.as_str());
        if !self.payload_vars.inline_option_payload_vars.contains(name) {
            return;
        }
        let PatternKind::TupleVariant { path, patterns } = &pattern.kind else {
            return;
        };
        if path.last().map(|s| s.as_str()) != Some("Some") {
            return;
        }
        if !patterns.iter().any(pattern_consumes_field) {
            return;
        }
        let Some(slot) = self.variables.get(name) else {
            return;
        };
        let Some(layout) = self.type_decls.enum_layouts.get("Option") else {
            return;
        };
        let i64_t = self.context.i64_type();
        // cap word of the `{ptr,len,cap}` payload: tag(0) + w0(1) + w1(2) +
        // w2/cap(3).
        if let Ok(cap_ptr) =
            self.builder
                .build_struct_gep(layout.llvm_type, slot.ptr, 3, "optpl.suppress.cap")
        {
            let _ = self.builder.build_store(cap_ptr, i64_t.const_int(0, false));
        }
    }

    /// Container-move sibling of `suppress_inline_option_payload_cleanup`
    /// (owned-temp slice 3p): `v.push(o)` where `o: Option[String]` bit-copies
    /// the option aggregate (tag + payload words) into the vec's buffer, and
    /// the per-element `karac_drop_Option_<payload>` now frees the payload
    /// there — so the source binding's `FreeInlineOptionPayload` would free
    /// the same buffer twice (SIGTRAP). Zero the source's cap word (option
    /// field 3) so its `cap > 0` guard skips; the container becomes the
    /// unique owner. The Option sibling of the push arm's
    /// `suppress_source_vec_cleanup_for_arg` cap-zeroing. No-op unless the
    /// arg is an identifier with an armed inline-Option-payload cleanup.
    /// Slice 3u: the BOXED sibling of the two inline moved-arg suppressors
    /// below — `v.push(o)` / `m.insert(k, o)` where `o` is a boxed-payload
    /// Option/Result binding (`boxed_enum_payload_vars`) bit-copies the
    /// `{tag, w0..}` aggregate into the container, whose per-element drop
    /// (3u) now owns the box — the source's `BoxedEnumDrop` must skip.
    /// Null the BOX WORD (field 1): the drop's null-guard then no-ops.
    /// Variant-agnostic (works for Result's Ok/Err alike) and branch-safe
    /// (a runtime store on this path only). A heapless live variant's w0 is
    /// data, but its `BoxedEnumDrop` tag-guard never reads it and the
    /// container's copy already captured the real words.
    /// Resolve a moved-arg name through the passthrough OWNERSHIP ALIAS before
    /// the `_for_moved_arg` suppressors below look it up. B-2026-08-06-29.
    ///
    /// `let x = idopt(d); peek(x);` double-freed on a DEFAULT -O2 build. The
    /// suppressors zero the named binding's payload so a callee that CONSUMES
    /// the argument is its only owner — which works for `peek(d)`, where `d` is
    /// itself armed. It found nothing for `peek(x)`, because B-2026-08-06-27
    /// deliberately does NOT arm `x`: the passthrough result aliases `d`, so
    /// `x` records `passthrough_owner_alias[x] = d` instead and `d` stays the
    /// sole owner. Nothing then disarmed `d` when its alias was consumed, so
    /// the callee freed the payload and `d`'s own cleanup freed it again.
    ///
    /// Resolving the alias makes the suppression land on the binding that
    /// actually owns the payload. This is the SAME operation the direct case
    /// already performs — ownership transferring to a consuming callee — not a
    /// new blanket zeroing of a source, which is the move this family's rows
    /// (-21, -27, -28) warn against. It fires only where a consume is already
    /// happening.
    ///
    /// One hop, not a chain: the alias map records a binding's immediate
    /// source, and a chained re-passthrough (`let y = idopt(x);`) records `y`
    /// against `x` — resolving repeatedly would be the correct generalization
    /// if that shape ever arms, but it does not today and an unbounded walk
    /// over a map that could contain a cycle is not worth adding unmeasured.
    pub(super) fn moved_arg_owner_name(&self, name: &str) -> String {
        self.payload_vars
            .passthrough_owner_alias
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }

    /// B-2026-08-09-8 — is `value` a bare identifier naming a binding that owns
    /// an inline `Option`/`Result` payload, i.e. is this `let p = o;` a
    /// whole-value MOVE of one? Returns the OWNING binding's name.
    ///
    /// The let-site's registration gate could not see this shape.
    /// `rhs_is_fresh_inline_enum` answers false for an identifier naming an
    /// existing binding — correctly, since it is not fresh — and neither
    /// passthrough detector matches a bare identifier either, so the
    /// destination got NO cleanup while the source kept its. That alone is
    /// balanced, but it is invisible to `scrutinee_is_inline_optres_local`,
    /// which is the membership test the caller-retains classifiers consult: a
    /// later `match p` therefore took the OWNED path and its arm binding freed
    /// the buffer at arm exit, while the source's untouched scope-exit action
    /// freed it again.
    ///
    /// Resolves through an existing passthrough alias, exactly as the
    /// suppressors do, so a rebind of a passthrough binding reports the real
    /// owner rather than the alias.
    pub(super) fn inline_optres_rebind_move_source(&self, value: &Expr) -> Option<String> {
        let ExprKind::Identifier(n) = &value.kind else {
            return None;
        };
        let owner = self.moved_arg_owner_name(n);
        (self
            .payload_vars
            .inline_option_payload_vars
            .contains(owner.as_str())
            || self
                .payload_vars
                .inline_result_payload_vars
                .contains(owner.as_str()))
        .then_some(owner)
    }

    pub(super) fn suppress_boxed_enum_payload_cleanup_for_moved_arg(&self, arg: &Expr) {
        let ExprKind::Identifier(name) = &arg.kind else {
            return;
        };
        // B-2026-08-06-29 — follow the passthrough ownership alias, so the
        // suppression lands on the binding that actually owns the payload.
        self.suppress_boxed_enum_payload_cleanup_for_owner(&self.moved_arg_owner_name(name));
    }

    /// The body of [`Self::suppress_boxed_enum_payload_cleanup_for_moved_arg`],
    /// keyed by an already-resolved OWNER name. Zeroes the box word (w0) alone,
    /// so the `BoxedEnumDrop`'s null check skips while every other channel
    /// sharing the slot keeps its own guard — which is why B-2026-08-06-9 leg A
    /// disarms a passthrough source through here rather than through the
    /// whole-slot zero.
    pub(super) fn suppress_boxed_enum_payload_cleanup_for_owner(&self, name: &str) {
        if !self.payload_vars.boxed_enum_payload_vars.contains(name) {
            return;
        }
        let Some(slot) = self.variables.get(name) else {
            return;
        };
        let BasicTypeEnum::StructType(enum_ty) = slot.ty else {
            return;
        };
        let i64_t = self.context.i64_type();
        if let Ok(w0_ptr) = self
            .builder
            .build_struct_gep(enum_ty, slot.ptr, 1, "boxpl.movearg.w0")
        {
            let _ = self.builder.build_store(w0_ptr, i64_t.const_int(0, false));
        }
    }

    pub(super) fn suppress_inline_option_payload_cleanup_for_moved_arg(&self, arg: &Expr) {
        let ExprKind::Identifier(name) = &arg.kind else {
            return;
        };
        // B-2026-08-06-29 — follow the passthrough ownership alias, so the
        // suppression lands on the binding that actually owns the payload.
        let name = &self.moved_arg_owner_name(name);
        if !self
            .payload_vars
            .inline_option_payload_vars
            .contains(name.as_str())
        {
            return;
        }
        let Some(slot) = self.variables.get(name.as_str()) else {
            return;
        };
        let Some(layout) = self.type_decls.enum_layouts.get("Option") else {
            return;
        };
        let i64_t = self.context.i64_type();
        if let Ok(cap_ptr) =
            self.builder
                .build_struct_gep(layout.llvm_type, slot.ptr, 3, "optpl.movearg.cap")
        {
            let _ = self.builder.build_store(cap_ptr, i64_t.const_int(0, false));
        }
    }

    /// `Result[T, E]` sibling of
    /// `suppress_inline_option_payload_cleanup_for_moved_arg` (slice 3q):
    /// `v.push(r)` where `r: Result[String, E]` bit-copies the result
    /// aggregate into the vec, whose per-element `karac_drop_Result_<ok>_<err>`
    /// now frees the live payload there — so the source binding's
    /// `FreeInlineResultPayload` would free the same buffer twice. Zero the
    /// source's cap word (field 3 — Ok and Err overlay the same w0..w2) so its
    /// `cap > 0` guard skips. No-op unless the arg is an identifier with an
    /// armed inline-Result-payload cleanup.
    pub(super) fn suppress_inline_result_payload_cleanup_for_moved_arg(&self, arg: &Expr) {
        let ExprKind::Identifier(name) = &arg.kind else {
            return;
        };
        // B-2026-08-06-29 — follow the passthrough ownership alias, so the
        // suppression lands on the binding that actually owns the payload.
        let name = &self.moved_arg_owner_name(name);
        if !self
            .payload_vars
            .inline_result_payload_vars
            .contains(name.as_str())
        {
            return;
        }
        let Some(slot) = self.variables.get(name.as_str()) else {
            return;
        };
        let Some(layout) = self.type_decls.enum_layouts.get("Result") else {
            return;
        };
        let i64_t = self.context.i64_type();
        if let Ok(cap_ptr) =
            self.builder
                .build_struct_gep(layout.llvm_type, slot.ptr, 3, "respl.movearg.cap")
        {
            let _ = self.builder.build_store(cap_ptr, i64_t.const_int(0, false));
        }
    }

    /// B-2026-08-05-3 — does this arm take OWNERSHIP of a named boxed TUPLE
    /// payload's interior? If so, its `BoxedEnumDrop` must be downgraded to a
    /// box-only free, or the arm's owner and the box both free the same buffer.
    /// Returns the scrutinee binding name to retract against.
    ///
    /// Two ways an arm takes it, needing different rules:
    ///
    ///   * a per-element DESTRUCTURE (`Some((v, k))`) ALWAYS does —
    ///     `bind_pattern_values` gives each heap leaf its own `track_vec_var`,
    ///     whatever the body then does with it. B-2026-07-18-3's fresh-temp
    ///     path reaches the same conclusion structurally, by never installing
    ///     an inner drop for this shape.
    ///   * a WHOLE-value binding (`Some(x)`) does so only when the arm actually
    ///     consumes it: `Some(x) => x.0` hands the element out and must
    ///     retract, while `Some(x) => x.0[0]` merely reads and must NOT — that
    ///     binding registers no owner, so retracting there re-opens the leak.
    ///
    /// Both error directions here are LEAKS, never a double free, which is why
    /// the classifier's conservative "consumed" default is safe on this site
    /// too even though it points the opposite way from the suppressors above.
    fn boxed_tuple_payload_arm_takes_ownership(
        &self,
        scrutinee: &Expr,
        pattern: &Pattern,
        arm_only_borrows: bool,
    ) -> Option<String> {
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return None;
        };
        if !self
            .payload_vars
            .boxed_enum_payload_vars
            .contains(name.as_str())
        {
            return None;
        }
        let PatternKind::TupleVariant { path, patterns } = &pattern.kind else {
            return None;
        };
        if !matches!(
            path.last().map(|s| s.as_str()),
            Some("Some") | Some("Ok") | Some("Err")
        ) {
            return None;
        }
        let destructures = patterns
            .iter()
            .any(|p| matches!(&p.kind, PatternKind::Tuple(_)));
        let whole_tuple_binding = patterns.iter().any(|p| {
            matches!(&p.kind, PatternKind::Binding(_))
                && self
                    .pattern_state
                    .pattern_binding_types
                    .get(&(p.span.offset, p.span.length))
                    .is_some_and(|t| t == "Tuple")
        });
        (destructures || (whole_tuple_binding && !arm_only_borrows)).then(|| name.clone())
    }

    /// The binding names a `Some`/`Ok`/`Err` arm introduces, for the borrow
    /// verdict in [`Self::boxed_tuple_payload_arm_takes_ownership`].
    fn variant_arm_binds(pattern: &Pattern) -> Vec<String> {
        let PatternKind::TupleVariant { patterns, .. } = &pattern.kind else {
            return Vec::new();
        };
        let mut binds = Vec::new();
        for p in patterns {
            collect_pattern_bindings(p, &mut binds);
        }
        binds
    }

    /// `match`-arm entry point for the retraction above.
    pub(super) fn retract_boxed_tuple_inner_drop_for_arm(
        &mut self,
        scrutinee: &Expr,
        pattern: &Pattern,
        body: &Expr,
        guard: Option<&Expr>,
    ) {
        let binds = Self::variant_arm_binds(pattern);
        let only_borrows = !binds.is_empty()
            && binds.iter().all(|v| {
                super::consume_class::binding_only_borrowed(v, body)
                    && guard.is_none_or(|g| super::consume_class::binding_only_borrowed(v, g))
            });
        if let Some(name) =
            self.boxed_tuple_payload_arm_takes_ownership(scrutinee, pattern, only_borrows)
        {
            self.clear_boxed_enum_inner_drop(&name);
        }
    }

    /// Block-body sibling, for the if-let `then_block` / while-let `body`
    /// scopes. `let`-else passes `None`: its bindings escape into the enclosing
    /// scope, so they always take ownership.
    pub(super) fn retract_boxed_tuple_inner_drop_for_block(
        &mut self,
        scrutinee: &Expr,
        pattern: &Pattern,
        block: Option<&crate::ast::Block>,
    ) {
        let binds = Self::variant_arm_binds(pattern);
        let only_borrows = block.is_some_and(|b| {
            !binds.is_empty()
                && binds
                    .iter()
                    .all(|v| super::consume_class::binding_only_borrowed_block(v, b))
        });
        if let Some(name) =
            self.boxed_tuple_payload_arm_takes_ownership(scrutinee, pattern, only_borrows)
        {
            self.clear_boxed_enum_inner_drop(&name);
        }
    }

    /// B-2026-08-05-3 — the binding names introduced by a `Result` arm that
    /// consumes a WHOLE-TUPLE payload out of a named scrutinee, else `None`.
    /// Every gate of `suppress_inline_result_payload_cleanup` plus the tuple
    /// restriction, so the consumption-gated callers below can ask "would the
    /// suppression apply, and to a tuple?" in one call.
    ///
    /// Restricted to a plain `Binding` the typechecker typed `Tuple`. A tuple
    /// PATTERN (`Ok((v, k))`) is deliberately excluded: it binds the ELEMENTS,
    /// and each heap element gets its own `track_vec_var` owner, so the source
    /// there must suppress exactly as it does today.
    fn result_tuple_payload_binds(
        &self,
        scrutinee: &Expr,
        pattern: &Pattern,
    ) -> Option<Vec<String>> {
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return None;
        };
        if !self
            .payload_vars
            .inline_result_payload_vars
            .contains(name.as_str())
        {
            return None;
        }
        let PatternKind::TupleVariant { path, patterns } = &pattern.kind else {
            return None;
        };
        if !matches!(path.last().map(|s| s.as_str()), Some("Ok") | Some("Err")) {
            return None;
        }
        if !patterns.iter().any(pattern_consumes_field) {
            return None;
        }
        let binds_a_tuple = patterns.iter().any(|p| {
            matches!(&p.kind, PatternKind::Binding(_))
                && self
                    .pattern_state
                    .pattern_binding_types
                    .get(&(p.span.offset, p.span.length))
                    .is_some_and(|t| t == "Tuple")
        });
        if !binds_a_tuple {
            return None;
        }
        let mut binds: Vec<String> = Vec::new();
        for pat in patterns {
            collect_pattern_bindings(pat, &mut binds);
        }
        (!binds.is_empty()).then_some(binds)
    }

    /// B-2026-08-05-3 — does a `Result` arm leave its `Ok(_)`/`Err(_)`-bound
    /// WHOLE-TUPLE payload only BORROWED? When it does, the source must keep
    /// its `FreeInlineResultPayload` armed, so
    /// `suppress_inline_result_payload_cleanup` must NOT fire.
    ///
    /// The tuple payload is the one shape with no binding-side owner: a
    /// struct payload is `track_struct_var`'d and a direct `String`/`Vec`
    /// payload is `track_vec_var`'d, but `bind_pattern_values` never
    /// `track_tuple_var`s a match binding. Disarming the source for a tuple
    /// therefore hands the buffer to nobody — `match r { Ok(x) =>
    /// println(f"{x.0[0]}") }` over a `Result[(Vec[i64], i64), i64]` leaked
    /// the element buffer while the `Option` carrier was clean, because
    /// `Option`'s suppression has been consumption-gated since B-2026-07-03-31
    /// (`arm_only_borrows_option_agg_payload`) and `Result`'s never was. This
    /// is that missing sibling, narrowed to the tuple so the struct and
    /// direct-heap payloads keep their unconditional suppression.
    ///
    /// Same conservative direction as the `Option` twin: the classifier
    /// defaults to "consumed", so a wrong answer keeps today's suppression —
    /// a leak, never a double-free.
    pub(super) fn arm_only_borrows_result_tuple_payload(
        &self,
        scrutinee: &Expr,
        pattern: &Pattern,
        body: &Expr,
        guard: Option<&Expr>,
    ) -> bool {
        let Some(binds) = self.result_tuple_payload_binds(scrutinee, pattern) else {
            return false;
        };
        binds.iter().all(|v| {
            super::consume_class::binding_only_borrowed(v, body)
                && guard.is_none_or(|g| super::consume_class::binding_only_borrowed(v, g))
        })
    }

    /// Block-body sibling of [`Self::arm_only_borrows_result_tuple_payload`]
    /// for the if-let `then_block` / while-let `body` scopes.
    pub(super) fn block_only_borrows_result_tuple_payload(
        &self,
        scrutinee: &Expr,
        pattern: &Pattern,
        block: &crate::ast::Block,
    ) -> bool {
        let Some(binds) = self.result_tuple_payload_binds(scrutinee, pattern) else {
            return false;
        };
        binds
            .iter()
            .all(|v| super::consume_class::binding_only_borrowed_block(v, block))
    }

    /// `Result[T, E]` sibling of `suppress_inline_option_payload_cleanup`.
    /// When the scrutinee is an identifier whose binding registered a
    /// `FreeInlineResultPayload` and the arm binds the `Ok`/`Err` payload
    /// out, zero the source `Result`'s `cap` word (field index 3 — `Ok` and
    /// `Err` payloads overlay the same `{ptr,len,cap}` at words w0/w1/w2) so
    /// the scope-exit free skips on the taken arm; the bound payload's own
    /// cleanup frees it once. The store lands in the arm's body block, so it
    /// only fires at runtime when that arm (= the live variant) is taken — a
    /// non-consuming `Ok(_)` / `Err(_)` / wildcard arm runs no suppression
    /// and the source free fires for the live payload.
    pub(super) fn suppress_inline_result_payload_cleanup(
        &self,
        scrutinee: &Expr,
        pattern: &Pattern,
    ) {
        // B-2026-08-08-25 — Result twin of the caller-retains early-return in
        // `suppress_inline_option_payload_cleanup`; same reasoning.
        if self
            .pattern_state
            .pattern_binding_source_retains_inline_payload
        {
            return;
        }
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return;
        };
        // B-2026-08-06-27 — Result twin of the forwarding in
        // `suppress_inline_option_payload_cleanup`: a passthrough result
        // binding owns nothing, so the disarm must land on its source.
        let name: &str = self
            .payload_vars
            .passthrough_owner_alias
            .get(name.as_str())
            .map_or(name.as_str(), |s| s.as_str());
        if !self.payload_vars.inline_result_payload_vars.contains(name) {
            return;
        }
        let PatternKind::TupleVariant { path, patterns } = &pattern.kind else {
            return;
        };
        let variant = path.last().map(|s| s.as_str());
        if variant != Some("Ok") && variant != Some("Err") {
            return;
        }
        if !patterns.iter().any(pattern_consumes_field) {
            return;
        }
        let Some(slot) = self.variables.get(name) else {
            return;
        };
        let Some(layout) = self.type_decls.enum_layouts.get("Result") else {
            return;
        };
        // B-2026-08-28-68 — `zero_result_payload_area` zeroes fields `1..n`,
        // i.e. EVERY payload word. Its own doc calls that a "safe superset",
        // and it is — while both sides are INLINE. When the side this arm
        // MATCHED is heap-BOXED, word 0 holds the box POINTER, and nulling it
        // makes the `BoxedEnumDrop` tag/pointer guard skip: the box and its
        // interior leak (measured 128 direct + 64 indirect bytes over two `Ok`
        // iterations, 31 allocs / 27 frees).
        //
        // A boxed side has no inline action to disarm in the first place — its
        // payload belongs to that `BoxedEnumDrop` — so the whole suppression is
        // a no-op there and the correct move is to leave the slot alone.
        //
        // The matched side's boxedness is read off the ARMED ACTION rather than
        // re-derived from the type: a `BoxedEnumDrop` on this slot guarded on
        // this variant's discriminant IS the statement "this side is boxed and
        // owned elsewhere". That also keeps the two in step by construction —
        // if the registration ever declines to box, there is no action to find
        // and the zeroing resumes.
        //
        // Zeroing only the overlay's `cap` word instead was tried and is WRONG:
        // a struct-with-heap inline payload cap-guards on the concrete struct's
        // own field offsets, which is exactly why this zeroes the whole area,
        // and disarming only `cap` left that walk armed beside the arm's
        // binding — an ASAN double-free.
        let matched_tag = layout.tags.get(variant.unwrap_or_default()).copied();
        let matched_side_is_boxed = matched_tag.is_some_and(|tag| {
            self.drop_rc.scope_cleanup_actions.iter().any(|frame| {
                frame.iter().any(|a| {
                    matches!(
                        a,
                        super::state::CleanupAction::BoxedEnumDrop {
                            enum_slot,
                            some_tag,
                            ..
                        } if *enum_slot == slot.ptr && *some_tag == tag
                    )
                })
            })
        });
        if matched_side_is_boxed {
            return;
        }
        self.zero_result_payload_area(layout.llvm_type, slot.ptr, "respl.suppress");
    }

    /// Suppress the scope-exit `FreeInlineOptionPayload` /
    /// `FreeInlineResultPayload` of an `Option`/`Result` BINDING that has just
    /// been MOVED whole into a struct-literal / enum-variant field
    /// (`TraitMethodNode { body: body }`, where `body: Option[Block]`). Unlike
    /// the `match`-arm suppressors above (which bind the payload OUT and zero
    /// just the inline `cap` word), a whole-value move hands the entire
    /// `Option`/`Result` — tag AND payload (an inline `{ptr,len,cap}` heap
    /// buffer OR a heap-boxed wide payload whose box pointer sits in word 1) —
    /// to the destination aggregate, which now solely owns it. Zero the source
    /// slot outright so BOTH guard shapes skip: the boxed cleanup's
    /// `tag == Some` check (word 0) and the inline cleanup's `cap > 0` check
    /// (word 3) both read zero. Gated on the binding carrying one of these
    /// inline/boxed cleanups (`inline_{option,result}_payload_vars`); a
    /// `shared`-inner `Option[shared T]` is NOT in those sets — it stays on the
    /// rc inc/dec balance the field-init paths already emit, untouched here.
    /// Surfaced by selfhost slice 3c-iv: `let mut body = Some(parse_block());
    /// TraitMethodNode { body, .. }` double-freed the boxed `Block` (the source
    /// binding's box drop + the returned node's downstream owner) → UAF.
    /// Is `value` a CALL that hands one of its arguments — an already-armed
    /// heap-BOXED `Option`/`Result` binding — straight back as its result?
    /// B-2026-08-06-21.
    ///
    /// A by-value argument whose callee returns it (`call_arg_flows_into_return`)
    /// deliberately keeps the CALLER's cleanup, because the callee handed the
    /// same value back. That is right for an INLINE payload — measured, an
    /// `Option[String]` passthrough is single-free — and for the entry-copied
    /// heap struct that path carves out, where two allocations genuinely exist.
    /// It is wrong for a BOXED one: nothing entry-copies a box, so the source
    /// binding and the result binding hold the SAME pointer and both
    /// `BoxedEnumDrop`s fire on it (confirmed in the emitted module — one
    /// `@malloc`, two `@free`s).
    ///
    /// The caller uses this to skip the RESULT binding's registration, leaving
    /// the source as sole owner. The other direction — zeroing the source and
    /// letting the result own it — was tried and is WRONG: when the result is
    /// discarded (`id(bound);`), the source is the only owner there is, and
    /// zeroing it turned a clean program into a 320-byte leak.
    ///
    /// Boxed-ONLY: an inline payload's caller-retains behaviour on this path is
    /// correct and tested, and must not be disturbed.
    ///
    /// Returns the SOURCE binding's name, so the let site can record the
    /// ownership alias as well as skip its own registration — the boxed peer of
    /// [`Self::call_passthrough_armed_inline_source`]. B-2026-08-06-9 leg A:
    /// -21 introduced the boxed skip and -27 the inline one, but only the
    /// inline site recorded an alias. That asymmetry was invisible while
    /// nothing else freed a boxed passthrough result — the source binding was
    /// the sole owner and its own cleanup balanced. Once the CALLEE owns a
    /// non-struct boxed payload (leg A), consuming the alias (`let r = id(b);
    /// classify(r);`) freed the box in the callee and again in the source's
    /// cleanup, an abort at `-O0`.
    ///
    /// CHAINED passthroughs resolve here rather than at lookup time. `let r1 =
    /// id(b); let r2 = id(r1);` hands `id` an argument that is not itself armed
    /// — `r1` is an alias — so the shared test above returns `None` and `r2`
    /// registers a drop of a box it does not own. Following the alias one hop
    /// at the RECORD site keeps every stored value a genuinely armed owner, so
    /// `moved_arg_owner_name` stays the single-hop lookup -29 built and no walk
    /// over a possibly-cyclic map is needed: a self-alias is impossible because
    /// a name that is its own owner is armed and takes the direct branch.
    pub(super) fn call_passthrough_armed_boxed_source(&self, value: &Expr) -> Option<String> {
        let ExprKind::Call { callee, args, .. } = &value.kind else {
            return None;
        };
        let ExprKind::Identifier(callee_name) = &callee.kind else {
            return None;
        };
        args.iter().enumerate().find_map(|(i, a)| {
            let ExprKind::Identifier(n) = &a.value.kind else {
                return None;
            };
            if !self.call_arg_flows_into_return(callee_name, i) {
                return None;
            }
            if self
                .payload_vars
                .boxed_enum_payload_vars
                .contains(n.as_str())
            {
                return Some(n.clone());
            }
            let owner = self
                .payload_vars
                .boxed_passthrough_owner_alias
                .get(n.as_str())?;
            self.payload_vars
                .boxed_enum_payload_vars
                .contains(owner.as_str())
                .then(|| owner.clone())
        })
    }

    /// NESTED-box sibling of [`Self::call_passthrough_armed_boxed_source`] —
    /// the `Result[Option[Wide], E]` shape whose box sits inside the inline
    /// payload area. B-2026-08-06-32.
    ///
    /// Same rule and same direction as the boxed one: `fn id(r) -> r` hands the
    /// box straight back, nothing entry-copies it, so the source binding and
    /// the result binding hold ONE pointer and only the source may own it.
    /// Without this the result's let site registers a second
    /// `NestedBoxedEnumDrop` and both fire — measured as a glibc double free at
    /// `-O0`, i.e. this rule turns a leak into corruption if omitted.
    ///
    /// Reads its own armed set, never `boxed_enum_payload_vars`: the two
    /// populations are disjoint by construction (a payload is boxed at a level
    /// or inline at it, never both) and the nested one deliberately sits
    /// outside the move rules.
    /// The binding that actually owns the nested box `value` evaluates to, or
    /// `None` if nothing armed does. B-2026-08-07-2.
    ///
    /// [`Self::call_passthrough_armed_nested_source`] answers the same question
    /// for the let site, but only one hop and only for a call whose argument is
    /// a bare identifier. That is enough where it is used and NOT enough at a
    /// by-value argument, where the callee is about to take ownership and every
    /// spelling that still aliases a live owner has to disarm it. Two got
    /// through the one-hop form and both were measured as a glibc `double free
    /// detected in tcache 2` at -O0:
    ///
    ///   * `cls(id(id(b)))` — the outer passthrough's argument is a CALL, so
    ///     the identifier match fails and the walk stops before reaching `b`.
    ///   * `let c = id(b); cls(c)` — `c` is an identifier but registers
    ///     nothing; the let site recorded it as an ALIAS of `b`, and the owner
    ///     is one map lookup away.
    ///
    /// So this resolves both directions to a fixpoint: through the alias map
    /// for an identifier, and through the passthrough argument for a call.
    /// The alias walk is bounded rather than trusting the map to be acyclic —
    /// a cycle here would hang the compiler, and the bound costs nothing since
    /// real chains are one or two hops.
    pub(super) fn nested_boxed_owner_source_of(&self, value: &Expr) -> Option<String> {
        match &value.kind {
            ExprKind::Identifier(n) => {
                let mut cur = n.clone();
                for _ in 0..8 {
                    if self
                        .payload_vars
                        .nested_boxed_payload_vars
                        .contains(cur.as_str())
                    {
                        return Some(cur);
                    }
                    match self
                        .payload_vars
                        .nested_boxed_passthrough_owner_alias
                        .get(cur.as_str())
                    {
                        Some(next) if *next != cur => cur = next.clone(),
                        _ => return None,
                    }
                }
                None
            }
            ExprKind::Call { callee, args, .. } => {
                let ExprKind::Identifier(callee_name) = &callee.kind else {
                    return None;
                };
                args.iter().enumerate().find_map(|(i, a)| {
                    self.call_arg_flows_into_return(callee_name, i)
                        .then(|| self.nested_boxed_owner_source_of(&a.value))
                        .flatten()
                })
            }
            _ => None,
        }
    }

    pub(super) fn call_passthrough_armed_nested_source(&self, value: &Expr) -> Option<String> {
        let ExprKind::Call { callee, args, .. } = &value.kind else {
            return None;
        };
        let ExprKind::Identifier(callee_name) = &callee.kind else {
            return None;
        };
        args.iter().enumerate().find_map(|(i, a)| {
            let ExprKind::Identifier(n) = &a.value.kind else {
                return None;
            };
            if !self.call_arg_flows_into_return(callee_name, i) {
                return None;
            }
            if self
                .payload_vars
                .nested_boxed_payload_vars
                .contains(n.as_str())
            {
                return Some(n.clone());
            }
            // A chained passthrough's argument is itself an unarmed result;
            // follow it one hop to the binding that does own the box.
            let owner = self
                .payload_vars
                .nested_boxed_passthrough_owner_alias
                .get(n.as_str())?;
            self.payload_vars
                .nested_boxed_payload_vars
                .contains(owner.as_str())
                .then(|| owner.clone())
        })
    }

    /// INLINE sibling of [`Self::call_passes_armed_boxed_binding_through`].
    /// B-2026-08-06-27.
    ///
    /// The boxed rule's doc originally asserted the inline path was "correct
    /// and tested" on this shape, on the strength of a single-free measurement
    /// of an `Option[String]` passthrough. That control used a string LITERAL,
    /// which lives in rodata and has nothing to free — so it could not have
    /// failed however wrong the ownership was. With a runtime-built payload the
    /// inline path double-frees exactly as the boxed one did, at the DEFAULT
    /// -O2 as well as -O0, and the arm shape is irrelevant (a non-binding
    /// `Some(_)` arm fails identically).
    ///
    /// Same remedy, same direction: the SOURCE binding stays sole owner and the
    /// result registers nothing. Not the reverse — a discarded result leaves
    /// the source as the only owner, so zeroing it would leak.
    pub(super) fn call_passthrough_armed_inline_source(&self, value: &Expr) -> Option<String> {
        self.call_passthrough_armed_source(value, &self.payload_vars.inline_option_payload_vars)
            .or_else(|| {
                self.call_passthrough_armed_source(
                    value,
                    &self.payload_vars.inline_result_payload_vars,
                )
            })
    }

    /// Superset of [`Self::call_passthrough_armed_inline_source`] that also
    /// consults the heap-BOXED payload set. B-2026-08-06-28.
    ///
    /// Kept separate rather than widening the inline detector, because that one
    /// is B-2026-08-06-27's `let`-site gate and widening it would change which
    /// bindings record a `passthrough_owner_alias` — a different decision, on a
    /// different row, with its own measured behaviour. This variant exists for
    /// the DISCARDED-temp registrars, where a boxed payload double-freed the
    /// same way its inline sibling did (`Option[Wide]` with two String fields,
    /// measured — while an `Option[shared T]` payload was already clean,
    /// because rc handles are not buffer-owned).
    pub(super) fn call_passthrough_armed_any_source(&self, value: &Expr) -> Option<String> {
        self.call_passthrough_armed_inline_source(value)
            .or_else(|| {
                self.call_passthrough_armed_source(
                    value,
                    &self.payload_vars.boxed_enum_payload_vars,
                )
            })
    }

    /// The BUILTIN sibling of [`Self::call_passthrough_armed_source`]:
    /// `Option`/`Result` `.map(f)`. Its ABSENT branch (`None`/`Err`) hands the
    /// receiver straight back — the hand-rolled scalar-inner lowering in
    /// `calls.rs` sets `absent_result = recv_struct.into()`, a SHALLOW copy of
    /// the receiver's `{ptr,len,cap}` words — so `<binding>.map(f)` ALIASES
    /// `<binding>`'s heap Err/None payload exactly as a user-function
    /// passthrough aliases its argument (B-2026-08-06-27/28/29). When the
    /// receiver is a NAMED binding armed in an inline Option/Result payload set,
    /// return its name so a consumer can disarm the SOURCE (`unwrap_or`, which
    /// frees the aliased payload on the Err/None branch) or a discarded-temp
    /// registrar can skip (leaving the source sole owner). B-2026-08-07-3.
    ///
    /// Gated to the SCALAR-inner (`Ok`/`Some` payload trivially-copyable) map
    /// lowering: a heap-inner map delegates to `compile_map_via_match_synthesis`
    /// (calls.rs), which owns the move-out and does not leave this shallow
    /// alias. Only `map` is recognized — the other Err/None-passthrough
    /// combinators are left for measured follow-up, not assumed by symmetry.
    ///
    /// B-2026-08-09-4 WROTE THAT GATE. The paragraph above described it from
    /// the day this detector landed, but the body never tested the inner type —
    /// every `.map` over an armed binding was claimed as an alias, heap inner
    /// included, and the synthesis result's freshly-produced payload was left
    /// with no owner at all. Worth remembering as a review lesson: a doc
    /// comment stating a narrowing is not evidence the narrowing exists.
    pub(super) fn map_passthrough_armed_source(&self, value: &Expr) -> Option<String> {
        let ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } = &value.kind
        else {
            return None;
        };
        if method != "map" || args.len() != 1 {
            return None;
        }
        if self.map_lowers_via_match_synthesis(value) {
            return None;
        }
        let ExprKind::Identifier(n) = &object.kind else {
            return None;
        };
        (self
            .payload_vars
            .inline_option_payload_vars
            .contains(n.as_str())
            || self
                .payload_vars
                .inline_result_payload_vars
                .contains(n.as_str()))
        .then(|| n.clone())
    }

    /// B-2026-08-09-4 — does `value` lower through
    /// `compile_map_via_match_synthesis` (calls.rs), the HEAP-inner arm of
    /// `Option`/`Result` `.map(f)`?
    ///
    /// The two `.map` lowerings own their result differently, and which one
    /// runs decides who frees the payload:
    ///
    /// * SCALAR inner — the hand-rolled arm compiles the absent branch as
    ///   `absent_result = recv_struct`, a SHALLOW copy of the receiver's words.
    ///   A heap `Err`/`None` payload is therefore ALIASED by the result, which
    ///   is exactly what `map_passthrough_armed_source` records (B-2026-08-07-3).
    /// * HEAP inner — the synthesis arm compiles `match o { Some(x) =>
    ///   Some(f(x)), None => None }` (`Ok`/`Err` for `Result`). The present
    ///   payload is FRESHLY produced by the mapper, the `Err` payload is MOVED
    ///   out of the receiver by its arm, and the match machinery suppresses the
    ///   receiver's own cleanup. The result OWNS both branches; nothing about it
    ///   aliases the source.
    ///
    /// Answers `false` when the receiver's payload type is unknown (no
    /// `method_unwrap_inner_types` entry), so a shape this family does not
    /// lower keeps today's behaviour rather than changing owner on a guess.
    pub(super) fn map_lowers_via_match_synthesis(&self, value: &Expr) -> bool {
        let ExprKind::MethodCall { method, args, .. } = &value.kind else {
            return false;
        };
        if method != "map" || args.len() != 1 {
            return false;
        }
        let key = (value.span.offset, value.span.length);
        self.span_tables
            .method_unwrap_inner_types
            .get(&key)
            .is_some_and(|inner_te| !super::vec_method::is_trivially_copyable_te(inner_te))
    }

    /// Shared shape test: is `value` a direct call to a named function, passing
    /// a binding that is present in `armed` in a position the callee returns?
    /// Returns the SOURCE binding's name so a caller can record the ownership
    /// alias as well as skip its own registration.
    ///
    /// `call_arg_flows_into_return` is conservative-TRUE on a mixed-path callee,
    /// so a callee that sometimes returns something else leaves the value
    /// unregistered here — a leak, the side this file errs toward over a double
    /// free.
    ///
    /// B-2026-08-29-4 — METHOD CALLS match too. This tested `ExprKind::Call`
    /// alone, so `impl Bx { fn take(ref self, o: Option[String]) ->
    /// Option[String] { o } }` over `let out = b.take(src);` recorded no alias:
    /// `out` registered its own inline-payload owner while `src` stayed armed,
    /// and both freed the SAME buffer once the result was consumed. Measured
    /// `free(): double free detected in tcache 2` on all three compiled
    /// backends, valgrind `Invalid free()`, 10 allocs against 11 frees — while
    /// the interpreter and the free-function twin (10 allocs, 10 frees, clean)
    /// were correct. One allocation, so nothing entry-copies here and the two
    /// bindings genuinely alias.
    ///
    /// The method callee is resolved through the span-keyed
    /// `method_callee_types` table, guarded on the method SEGMENT matching this
    /// call: chained calls share one aliased span key (the collision note in
    /// `method_call.rs`), and `lazyframe.rs` guards the same table the same way.
    /// `fn_returns_param` is asked on the RECEIVER-EXCLUDING index, because
    /// `find_function_ast` hands back the raw AST method whose `params` exclude
    /// the receiver — verified by instrumentation, not assumed.
    ///
    /// Asked DIRECTLY rather than through `call_arg_flows_into_return`, whose
    /// `Item::Function`-only scan answers false for a method key. Widening that
    /// shared predicate would move seven other consumers at once; this reads the
    /// same predicate it does, on the one shape this site needs.
    fn call_passthrough_armed_source(
        &self,
        value: &Expr,
        armed: &std::collections::HashSet<String>,
    ) -> Option<String> {
        match &value.kind {
            ExprKind::Call { callee, args, .. } => {
                let ExprKind::Identifier(callee_name) = &callee.kind else {
                    return None;
                };
                args.iter().enumerate().find_map(|(i, a)| {
                    let ExprKind::Identifier(n) = &a.value.kind else {
                        return None;
                    };
                    (armed.contains(n.as_str()) && self.call_arg_flows_into_return(callee_name, i))
                        .then(|| n.clone())
                })
            }
            ExprKind::MethodCall { method, args, .. } => {
                let key = (value.span.offset, value.span.length);
                let qualified = self.span_tables.method_callee_types.get(&key)?;
                if qualified.rsplit_once('.').map(|(_, m)| m) != Some(method.as_str()) {
                    return None;
                }
                let program = self.program_snapshot.as_deref()?;
                let f = super::declarations::find_function_ast(program, qualified)?;
                args.iter().enumerate().find_map(|(i, a)| {
                    let ExprKind::Identifier(n) = &a.value.kind else {
                        return None;
                    };
                    (armed.contains(n.as_str()) && crate::ast::fn_returns_param(f, i))
                        .then(|| n.clone())
                })
            }
            _ => None,
        }
    }

    pub(super) fn suppress_inline_option_result_binding_move(&self, value: &Expr) {
        self.suppress_inline_option_result_binding_move_impl(value, false);
    }

    /// B-2026-08-30-8 — the ESCAPING entry point: `value` is being RETURNED, so
    /// the caller becomes the payload's owner and the source's scope-exit free
    /// must go, even for a source a read-only `match` arm left owning its
    /// payload (`inline_optres_retained_sources`).
    ///
    /// That retention veto is right at every CONSUMING position — the premise
    /// there is "some callee already took the buffer", which a borrowing arm
    /// never did, so zeroing the sole owner's slot would leak (B-2026-08-08-25
    /// leg 1). A return has the opposite premise and does not need one: handing
    /// the header out IS the transfer, so exactly one owner remains either way.
    ///
    /// Only the BARE-IDENTIFIER shape overrides the veto. A `.map(f).unwrap_or(d)`
    /// chain in return position is still a consuming expression whose source may
    /// legitimately have kept its buffer, so it keeps the veto and reaches the
    /// caller through the chain's own value, not through this slot.
    pub(super) fn suppress_inline_option_result_binding_escape(&self, value: &Expr) {
        self.suppress_inline_option_result_binding_move_impl(value, true);
    }

    fn suppress_inline_option_result_binding_move_impl(&self, value: &Expr, escaping: bool) {
        // Resolve the OWNING binding whose scope-exit payload free must be
        // disarmed because the caller (`unwrap_or`) has taken and freed the
        // aliased Err/None payload. Two receiver shapes reach here:
        //   • a NAMED binding (`r.unwrap_or(d)`) — B-2026-08-06-29's
        //     user-function-arg path, resolved through the passthrough alias;
        //   • `<binding>.map(f).unwrap_or(d)` (B-2026-08-07-3) — the `.map`
        //     Err/None branch aliases `<binding>`'s payload, and `unwrap_or`
        //     freed that alias, so the SOURCE binding must be disarmed or its
        //     scope-exit `FreeInlineResultPayload`/`FreeInlineOptionPayload`
        //     double-frees the same buffer.
        let name = match &value.kind {
            ExprKind::Identifier(name) => self.moved_arg_owner_name(name),
            _ => match self.map_passthrough_armed_source(value) {
                Some(src) => src,
                None => return,
            },
        };
        let name = &name;
        // B-2026-08-08-25 leg 1: a source a read-only match left OWNING its
        // payload has no other owner to hand off to, so disarming it here
        // leaks. The premise of this disarm — "the map's arm already took the
        // buffer" — only holds while the arm is on the transfer path.
        //
        // B-2026-08-30-8: a RETURN of the binding itself is on that path by
        // construction — the caller receives the header and owns the buffer —
        // so the veto does not apply there. See
        // `suppress_inline_option_result_binding_escape` for why the override is
        // limited to the bare-identifier shape.
        let escaping_bare_binding = escaping && matches!(&value.kind, ExprKind::Identifier(_));
        if !escaping_bare_binding
            && self
                .drop_rc
                .inline_optres_retained_sources
                .contains(name.as_str())
        {
            return;
        }
        let in_option = self
            .payload_vars
            .inline_option_payload_vars
            .contains(name.as_str());
        let in_result = self
            .payload_vars
            .inline_result_payload_vars
            .contains(name.as_str());
        // `boxed_enum_payload_vars` covers the heap-BOXED wide payload
        // (`Option[Block]`) whose `BoxedEnumDrop` guards on `tag == Some`; the
        // two inline sets cover the inline `{ptr,len,cap}` heap payload whose
        // free guards on `cap > 0`. Zeroing the whole slot below neutralizes
        // every shape's guard at once.
        let in_boxed = self
            .payload_vars
            .boxed_enum_payload_vars
            .contains(name.as_str());
        if !in_option && !in_result && !in_boxed {
            return;
        }
        let Some(slot) = self.variables.get(name.as_str()).copied() else {
            return;
        };
        let layout_name = if in_result { "Result" } else { "Option" };
        let Some(layout) = self.type_decls.enum_layouts.get(layout_name) else {
            return;
        };
        let _ = self
            .builder
            .build_store(slot.ptr, layout.llvm_type.const_zero());
    }

    /// B-2026-07-30-11 (Option/Result leg) — retract a named `Option`/`Result`
    /// binding's payload-BODIES action (`__karac_dropelems_opt_*` /
    /// `__karac_dropelems_res_*`) when a `match`/`if let` arm binds the
    /// payload out. The arm's binding owns the resource from then on; without
    /// the retraction the source's walk runs the body a second time — over
    /// data the memory-side tag-zero/cap-zero suppressors have already
    /// cleared. SHAPE-gated only (a `Some`/`Ok`/`Err` pattern whose payload
    /// position consumes): the retraction is a no-op for a binding that
    /// registered no walker, and the interpreter's twin
    /// (`pattern_consumes_user_drop_payload`'s Option/Result arm) uses the
    /// identical shape rule, so the two backends disarm the same cases.
    /// Static like every compile-time retraction here: one consuming arm
    /// disarms every path, and a non-taken arm's residual is a leak — the
    /// safe side.
    pub(super) fn suppress_optres_payload_bodies_for_match(
        &mut self,
        scrutinee: &Expr,
        pattern: &Pattern,
    ) {
        let name = match &scrutinee.kind {
            ExprKind::Identifier(n) => n.clone(),
            ExprKind::SelfValue => "self".to_string(),
            _ => return,
        };
        let PatternKind::TupleVariant { path, patterns } = &pattern.kind else {
            return;
        };
        if !matches!(
            path.last().map(|s| s.as_str()),
            Some("Some") | Some("Ok") | Some("Err")
        ) {
            return;
        }
        if !patterns
            .iter()
            .any(|sub| self.optres_sub_takes_drop_bearing(sub))
        {
            return;
        }
        // B-2026-09-02-14 — EDGE-SENSITIVE since this row, and the builder is
        // positioned in the arm's own block at every one of the four call
        // sites, so the `false` store lands on the hit edge alone. The static
        // removal it replaces applied to every path out of the construct,
        // including the one where the pattern did not match and no binding
        // exists to run the body: `let r = mkerr(); if let Ok(w) = r { … }`
        // ran `W`'s `Drop` body nowhere, on all four surfaces, while the same
        // local with no `if let` at all ran it correctly. The place keeps the
        // walk it never gave away, and the NLL machinery already fires it at
        // the place's real last use — which is what makes a LATER use
        // (`… ; match r { … }`) still run exactly one body, at the later site.
        //
        // Falls back to the removal when there is no flag to be had (no armed
        // action, or no current function), which is byte-identical to before.
        if let Some(flag) = self.optres_payload_bodies_flag_for(&name) {
            let _ = self
                .builder
                .build_store(flag, self.context.bool_type().const_int(0, false));
            return;
        }
        self.suppress_container_elem_bodies_for_var(&name);
    }

    /// B-2026-09-02-8 — does an `Option`/`Result` payload sub-pattern bind out
    /// a position whose DECLARED type runs a user `Drop` body?
    ///
    /// The caller's shape-only test (`pattern_consumes_field` on the whole
    /// sub-pattern) over-approximates. That is harmless when the SOURCE owes
    /// nothing, and wrong for a PARTIAL destructure of a payload that does:
    /// `let Some(H { n, .. }) = e else { … }`, with `n: i64` beside a
    /// `Drop`-bearing `r: R`, retracted `e`'s container-element bodies while
    /// the escaped binding `n` owned nothing — so the body ran on NO backend,
    /// an agreed-and-wrong answer no A/B gate could see.
    ///
    /// A BARE binding keeps the shape answer, deliberately: `Some(r)` names a
    /// payload whose declared type is a generic parameter, invisible here, and
    /// there the over-approximation is genuinely a no-op — an `Option[i64]`
    /// source registers no element-bodies action to retract. Only a
    /// DESTRUCTURE has field types to consult, and only a destructure can name
    /// a position the source's walk was covering for.
    ///
    /// Interpreter twin: `optres_sub_takes_drop_bearing` in `pattern_match.rs`,
    /// arm-for-arm the same and reading the same two declaration sources.
    fn optres_sub_takes_drop_bearing(&self, sub: &Pattern) -> bool {
        let PatternKind::Struct { path, fields, .. } = &sub.kind else {
            return pattern_consumes_field(sub);
        };
        // An unknown name (a builtin, an opaque type) has no field types to
        // read, so it keeps the shape answer rather than silently declining.
        let Some(decls) = self.destructured_field_decls(path) else {
            return pattern_consumes_field(sub);
        };
        fields.iter().any(|fp| {
            fp.pattern.as_ref().is_none_or(pattern_consumes_field)
                && decls.iter().any(|(n, te)| {
                    n.as_deref() == Some(fp.name.as_str()) && self.elem_te_runs_user_drop(te)
                })
        })
    }

    /// Field declarations behind a struct-destructure pattern's path — a plain
    /// `struct S { .. }` (`["S"]`) or an enum STRUCT VARIANT (`["E", "A"]`).
    /// `None` when no such type is declared, which is what sends
    /// [`Self::optres_sub_takes_drop_bearing`] back to the shape-only answer.
    ///
    /// Program snapshot then baked stdlib, the same two sources
    /// `struct_field_type_exprs` and `enum_variant_field_type_exprs` consult.
    fn destructured_field_decls(
        &self,
        path: &[String],
    ) -> Option<Vec<(Option<String>, crate::ast::TypeExpr)>> {
        fn variant(
            items: &[Item],
            enum_name: &str,
            variant: &str,
        ) -> Option<Vec<(Option<String>, crate::ast::TypeExpr)>> {
            items.iter().find_map(|item| match item {
                Item::EnumDef(e) if e.name == enum_name => e
                    .variants
                    .iter()
                    .find(|v| v.name == variant)
                    .map(|v| match &v.kind {
                        VariantKind::Unit => Vec::new(),
                        VariantKind::Tuple(tys) => tys.iter().map(|t| (None, t.clone())).collect(),
                        VariantKind::Struct(fs) => fs
                            .iter()
                            .map(|f| (Some(f.name.clone()), f.ty.clone()))
                            .collect(),
                    }),
                _ => None,
            })
        }
        fn strukt(
            items: &[Item],
            name: &str,
        ) -> Option<Vec<(Option<String>, crate::ast::TypeExpr)>> {
            items.iter().find_map(|item| match item {
                Item::StructDef(s) if s.name == name => Some(
                    s.fields
                        .iter()
                        .map(|f| (Some(f.name.clone()), f.ty.clone()))
                        .collect(),
                ),
                _ => None,
            })
        }
        let last = path.last()?;
        if path.len() >= 2 {
            let en = &path[path.len() - 2];
            if let Some(d) = self
                .program_snapshot
                .as_ref()
                .and_then(|p| variant(&p.items, en, last))
                .or_else(|| {
                    crate::prelude::STDLIB_PROGRAMS
                        .iter()
                        .find_map(|(_, p)| variant(&p.items, en, last))
                })
            {
                return Some(d);
            }
        }
        self.program_snapshot
            .as_ref()
            .and_then(|p| strukt(&p.items, last))
            .or_else(|| {
                crate::prelude::STDLIB_PROGRAMS
                    .iter()
                    .find_map(|(_, p)| strukt(&p.items, last))
            })
    }

    /// B-2026-08-04-2 — a whole-payload match binding taken out of a
    /// heap-BOXED `Option`/`Result` is MOVING on: hand the box's interior to
    /// the destination by clearing the source `BoxedEnumDrop`'s
    /// `inner_drop_fn`, leaving it a BOX-ONLY free.
    ///
    /// The binding is an unboxed COPY of the box's `{ptr,len,cap}` words, so
    /// after the move the destination and the box both believe they own those
    /// buffers and both free them — `let w = W { r: r }`, `let x = r`,
    /// `v.push(r)`, and an arm returning the binding as the match's value all
    /// abort under glibc. Dropping the inner walk makes the destination the
    /// sole owner while the box allocation itself is still reclaimed.
    ///
    /// Retracting the WALK rather than cap-zeroing the box's words is
    /// load-bearing, and the cap-zero was tried first: the box's words are also
    /// what the re-homed payload-BODIES walk reads (B-2026-08-02-25 /
    /// B-2026-08-04-1 point it at the source slot precisely so a mutating Drop
    /// body lands on the storage the later free sees). Zeroing them made the
    /// body print an empty string — `zero_struct_move_caps` clears `len`
    /// alongside `cap` for this shape — turning a double-free into a silent
    /// wrong-value. Clearing the action's fn pointer changes no data, so the
    /// body still reads the live payload.
    ///
    /// Static, like every compile-time retraction here: a move inside a
    /// conditional disarms on all paths, which can only under-free — the safe
    /// side. A no-op for any name not recorded as a boxed-payload view.
    /// B-2026-08-29-2 — retract a boxed binding's LEAF interior drop when an
    /// arm binds the very value that drop frees.
    ///
    /// The leaf drop (registered at the let site) exists because nothing else
    /// frees the innermost payload of a doubly-nested `Option`/`Result`. That
    /// is true only while nobody NAMES that payload. A pattern like
    /// `Some(Some(Some(s)))` binds it out, and the arm's own binding already
    /// owns and frees it — so leaving the box's drop armed frees it twice.
    /// Measured: without this, `asan_direct_boxed_enum_chain_frees_every_envelope`
    /// and `asan_struct_field_boxed_heapless_option_envelope_owned` both abort
    /// with an AddressSanitizer double-free.
    ///
    /// DEPTH IS THE WHOLE DIFFICULTY, and a coarse "the pattern binds any name"
    /// rule is wrong in the expensive direction — it retracts for
    /// `match vv { Some(o) => … }`, where `o` is an intermediate ENVELOPE whose
    /// binding does NOT take over the leaf, and every one of those shapes goes
    /// back to leaking. So the comparison is against
    /// `boxed_leaf_owning_depth`, recorded at registration from the same walk
    /// that chose the drop: only a binding at exactly that depth is the owner.
    ///
    /// Retraction is monotone and compile-time, so calling it once per arm is
    /// right even though only one arm runs: a `match` with a consuming arm and
    /// a wildcard arm keeps the leak on the wildcard path, which is the
    /// pre-existing behaviour and the safe direction — the alternative would be
    /// a double free on the consuming one.
    pub(super) fn retract_boxed_leaf_drop_for_consuming_pattern(
        &mut self,
        scrutinee: &Expr,
        pattern: &Pattern,
    ) {
        if self.pattern_state.pattern_binding_is_borrow {
            return;
        }
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return;
        };
        // B-2026-08-29-18 — the NESTED family retracts on ANY binding, not at a
        // measured depth, and the asymmetry is a property of what the source
        // can name rather than a loosening.
        //
        // For a DIRECT box the levels between the binding and the leaf are
        // `coerce_to_payload_words` ENVELOPES: the program cannot name one, so
        // an intermediate binding hands nothing over and only a binding at the
        // owning depth is an owner. For a NESTED box the inline payload IS an
        // `Option`/`Result` the program names directly — `match vv { Ok(o) =>
        // .. }` binds it, and `o` carries the box POINTER with it, so that one
        // binding takes over the box and everything under it.
        //
        // Measured: `Result[Option[Option[String]], i64]` matched
        // `Ok(o) => match o { Some(i) => match i { Some(s) => .. } }` is clean
        // today because the arm chain owns and frees the `String`; keeping the
        // leaf drop armed there is an AddressSanitizer double free. Retracting
        // on any binding is conservative in the only safe direction — it can
        // leave the pre-existing leak, never add a free — and the shapes this
        // row is about (`Ok(_)`, or no match at all) bind nothing.
        let retract = if self
            .payload_vars
            .nested_boxed_payload_vars
            .contains(name.as_str())
        {
            Self::pattern_binds_any_name(pattern)
        } else {
            let Some(depth) = self
                .payload_vars
                .boxed_leaf_owning_depth
                .get(name.as_str())
                .copied()
            else {
                return;
            };
            Self::pattern_binds_name_at_depth(pattern, depth)
        };
        if !retract {
            return;
        }
        let Some(slot) = self.variables.get(name.as_str()).copied() else {
            return;
        };
        for frame in self.drop_rc.scope_cleanup_actions.iter_mut() {
            for action in frame.iter_mut() {
                match action {
                    super::state::CleanupAction::BoxedEnumDrop {
                        enum_slot,
                        inner_drop_fn,
                        ..
                    } if *enum_slot == slot.ptr => *inner_drop_fn = None,
                    super::state::CleanupAction::NestedBoxedEnumDrop {
                        enum_slot,
                        leaf_drop_fn,
                        ..
                    } if *enum_slot == slot.ptr => *leaf_drop_fn = None,
                    _ => {}
                }
            }
        }
    }

    /// Does `pattern` bind a NAME exactly `depth` variant levels down?
    ///
    /// Depth 0 is the pattern itself, so `Some(Some(s))` binds `s` at depth 2.
    /// A wildcard binds nothing, which is the case that must keep the leaf drop
    /// armed — `Some(Some(_))` names no owner, so the box is still the only one.
    /// `Or` alternatives are searched independently: any alternative that binds
    /// the leaf makes the arm a potential owner.
    /// Does `pattern` bind a NAME anywhere below its own head?
    ///
    /// The NESTED-box rule (see `retract_boxed_leaf_drop_for_consuming_pattern`):
    /// there the inline payload is a type the program names, so any binding
    /// inside the variant pattern carries the box pointer out with it.
    fn pattern_binds_any_name(pattern: &Pattern) -> bool {
        match &pattern.kind {
            PatternKind::Binding(_) => true,
            PatternKind::AtBinding { .. } => true,
            PatternKind::Or(alts) => alts.iter().any(Self::pattern_binds_any_name),
            PatternKind::TupleVariant { patterns, .. } => {
                patterns.iter().any(Self::pattern_binds_any_name)
            }
            PatternKind::Tuple(elems) => elems.iter().any(Self::pattern_binds_any_name),
            // A shorthand field (`Foo { x }`) carries no sub-pattern and binds
            // the name directly, so `None` counts as a binding.
            PatternKind::Struct { fields, .. } => fields
                .iter()
                .any(|f| f.pattern.as_ref().is_none_or(Self::pattern_binds_any_name)),
            _ => false,
        }
    }

    fn pattern_binds_name_at_depth(pattern: &Pattern, depth: usize) -> bool {
        match &pattern.kind {
            PatternKind::Binding(_) => depth == 0,
            PatternKind::AtBinding {
                name: _,
                pattern,
                by_ref,
            } => (depth == 0 && !*by_ref) || Self::pattern_binds_name_at_depth(pattern, depth),
            PatternKind::Or(alts) => alts
                .iter()
                .any(|a| Self::pattern_binds_name_at_depth(a, depth)),
            PatternKind::TupleVariant { patterns, .. } => {
                depth > 0
                    && patterns
                        .iter()
                        .any(|p| Self::pattern_binds_name_at_depth(p, depth - 1))
            }
            _ => false,
        }
    }

    pub(super) fn suppress_boxed_payload_view_move(&mut self, value: &Expr) {
        let ExprKind::Identifier(name) = &value.kind else {
            return;
        };
        let Some(slot) = self
            .payload_vars
            .boxed_optres_payload_view_vars
            .get(name.as_str())
            .copied()
        else {
            return;
        };
        for frame in self.drop_rc.scope_cleanup_actions.iter_mut() {
            for action in frame.iter_mut() {
                if let super::state::CleanupAction::BoxedEnumDrop {
                    enum_slot,
                    inner_drop_fn,
                    ..
                } = action
                {
                    if *enum_slot == slot {
                        *inner_drop_fn = None;
                    }
                }
            }
        }
    }

    /// `Option[Map]`/`Option[Set]` sibling of
    /// `suppress_inline_option_payload_cleanup`. The inline handle payload
    /// has no `cap` word to zero, so a `match`/`if let` arm that binds the
    /// `Some` payload out instead overwrites the source tag with `None` —
    /// the `FreeInlineOptionMapPayload` tag-guard then skips. The store lands
    /// in the arm body (only fires when the consuming arm is taken). This
    /// prevents a double-free when the bound map is re-moved into a
    /// `Vec[Map]` (which then owns + frees it); a simple in-scope use of the
    /// bound map keeps leaking — that's the separate deferred match-bound-Map
    /// tracking gap, never a double-free.
    pub(super) fn suppress_inline_option_map_payload_cleanup(
        &self,
        scrutinee: &Expr,
        pattern: &Pattern,
    ) {
        // B-2026-08-30-47 — the `Map`/`Set` copy of the guard the inline
        // suppressor has carried since B-2026-08-08-25, and for the same
        // reason: suppression TRANSFERS the payload to the arm binding, so
        // when the classifier proved the arms only borrow it there is nobody
        // to transfer to, and the source's tag is set to `None` anyway.
        //
        // It is the `if let` spelling this catches, which is worth stating
        // because the `match` spelling does NOT need it — that path reaches
        // this suppressor only when the bindings are owned. Measured: with
        // the membership widening below but WITHOUT this guard,
        // `if let Some(p) = a { … }` over an `Option[Map]` still read back
        // `none` on the second `if let`, while every `match` shape passed.
        //
        // The struct/enum sibling `suppress_inline_option_agg_payload_cleanup`
        // deliberately has NO copy of this: its `if let` call site is already
        // gated on `block_only_borrows_option_agg_payload` (B-2026-07-03-31),
        // so a guard here would be unreachable. Adding one "for symmetry" was
        // tried and measured to change nothing.
        if self
            .pattern_state
            .pattern_binding_source_retains_inline_payload
        {
            return;
        }
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return;
        };
        if !self
            .payload_vars
            .inline_option_map_payload_vars
            .contains(name.as_str())
        {
            return;
        }
        let PatternKind::TupleVariant { path, patterns } = &pattern.kind else {
            return;
        };
        if path.last().map(|s| s.as_str()) != Some("Some") {
            return;
        }
        if !patterns.iter().any(pattern_consumes_field) {
            return;
        }
        let Some(slot) = self.variables.get(name.as_str()) else {
            return;
        };
        let Some(layout) = self.type_decls.enum_layouts.get("Option") else {
            return;
        };
        let i64_t = self.context.i64_type();
        let none_tag = layout.tags.get("None").copied().unwrap_or(0);
        if let Ok(tag_ptr) =
            self.builder
                .build_struct_gep(layout.llvm_type, slot.ptr, 0, "optmap.suppress.tag")
        {
            let _ = self
                .builder
                .build_store(tag_ptr, i64_t.const_int(none_tag, false));
        }
    }

    /// `Option[<user struct/enum>]` sibling of
    /// `suppress_inline_option_map_payload_cleanup` (B-2026-07-03-27). When a
    /// `match`/`if let` arm binds the `Some` payload out of a binding whose
    /// scope-exit `EnumDrop` runs `karac_drop_Option_<payload>` (tracked in
    /// `inline_option_agg_payload_vars`), the bound payload's own cleanup
    /// (`track_enum_var` / `track_struct_var` on the leaf) now frees it — so the
    /// source drop must skip. Like the `Option[Map]` case the payload has no
    /// `cap` word to zero, so overwrite the source tag with `None`: the
    /// `emit_option_drop_fn` tag-guard then no-ops. The store lands in the
    /// consuming arm body, so a non-consuming `Some(_)` / `None` arm leaves the
    /// source drop armed.
    /// If disarming the source inline-`Option`-aggregate payload drop WOULD
    /// apply for this scrutinee+pattern (every gate of
    /// `suppress_inline_option_agg_payload_cleanup` passes), return the OWNING
    /// binding names the `Some(_)` arm introduces; else `None`. Shared by the
    /// consumption-gated callers below.
    fn option_agg_payload_binds(&self, scrutinee: &Expr, pattern: &Pattern) -> Option<Vec<String>> {
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return None;
        };
        if !self
            .payload_vars
            .inline_option_agg_payload_vars
            .contains(name.as_str())
        {
            return None;
        }
        let PatternKind::TupleVariant { path, patterns } = &pattern.kind else {
            return None;
        };
        if path.last().map(|s| s.as_str()) != Some("Some") {
            return None;
        }
        if !patterns.iter().any(pattern_consumes_field) {
            return None;
        }
        // B-2026-08-05-3 — a DESTRUCTURING sub-pattern (`Some((v, k))`) binds
        // the payload's ELEMENTS, and `bind_pattern_values` gives each heap
        // element its own `track_vec_var` owner. The caller-retains gate below
        // asks whether the arm only READS the bindings, which is the wrong
        // question once they already own their buffers: answering "borrow-only"
        // leaves the SOURCE armed as well and both free the same pointer.
        // Only a WHOLE-value binding — which registers no owner of its own — is
        // eligible for the gate.
        //
        // Restricted to a TUPLE sub-pattern on purpose. A tuple payload only
        // entered this channel with B-2026-08-05-3's let-site registration, so
        // this cannot change how any pre-existing struct / enum payload behaves.
        if patterns
            .iter()
            .any(|p| matches!(&p.kind, PatternKind::Tuple(_)))
        {
            return None;
        }
        let mut binds: Vec<String> = Vec::new();
        for pat in patterns {
            collect_pattern_bindings(pat, &mut binds);
        }
        (!binds.is_empty()).then_some(binds)
    }

    /// Phase 1 (caller-retains model, B-2026-07-03-31): does an arm leave the
    /// `Some(_)`-bound inline-`Option`-aggregate payload ONLY BORROWED — never
    /// moved out / escaping? When true, the source retains ownership and its
    /// payload drop must stay armed, so `suppress_inline_option_agg_payload_cleanup`
    /// must NOT fire (else the payload's inner heap leaks — e.g.
    /// `Some(v) => ident_len(v)`, where `ident_len` entry-copies its owned
    /// param). Consults the consumption classifier over the arm body (and
    /// guard). The classifier's conservative default is "consumed", so a wrong
    /// answer only keeps the prior (suppress) behavior — never a double-free.
    /// The `match` / if-let / while-let callers pass an analyzable body; the
    /// let-else caller (whose bindings escape into the enclosing scope) keeps
    /// the unconditional suppression.
    pub(super) fn arm_only_borrows_option_agg_payload(
        &self,
        scrutinee: &Expr,
        pattern: &Pattern,
        body: &Expr,
        guard: Option<&Expr>,
    ) -> bool {
        let Some(binds) = self.option_agg_payload_binds(scrutinee, pattern) else {
            return false;
        };
        binds.iter().all(|v| {
            super::consume_class::binding_only_borrowed(v, body)
                && guard.is_none_or(|g| super::consume_class::binding_only_borrowed(v, g))
        })
    }

    /// Block-body sibling of [`Self::arm_only_borrows_option_agg_payload`] for
    /// the if-let `then_block` / while-let `body` scopes.
    pub(super) fn block_only_borrows_option_agg_payload(
        &self,
        scrutinee: &Expr,
        pattern: &Pattern,
        block: &crate::ast::Block,
    ) -> bool {
        let Some(binds) = self.option_agg_payload_binds(scrutinee, pattern) else {
            return false;
        };
        binds
            .iter()
            .all(|v| super::consume_class::binding_only_borrowed_block(v, block))
    }

    pub(super) fn suppress_inline_option_agg_payload_cleanup(
        &self,
        scrutinee: &Expr,
        pattern: &Pattern,
    ) {
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return;
        };
        if !self
            .payload_vars
            .inline_option_agg_payload_vars
            .contains(name.as_str())
        {
            return;
        }
        let PatternKind::TupleVariant { path, patterns } = &pattern.kind else {
            return;
        };
        if path.last().map(|s| s.as_str()) != Some("Some") {
            return;
        }
        if !patterns.iter().any(pattern_consumes_field) {
            return;
        }
        let Some(slot) = self.variables.get(name.as_str()) else {
            return;
        };
        let Some(layout) = self.type_decls.enum_layouts.get("Option") else {
            return;
        };
        let i64_t = self.context.i64_type();
        let none_tag = layout.tags.get("None").copied().unwrap_or(0);
        if let Ok(tag_ptr) =
            self.builder
                .build_struct_gep(layout.llvm_type, slot.ptr, 0, "optagg.suppress.tag")
        {
            let _ = self
                .builder
                .build_store(tag_ptr, i64_t.const_int(none_tag, false));
        }
    }

    /// Slice 3t: disarm the BOXED payload's consumed-field frees when a
    /// match/if-let arm STRUCT-DESTRUCTURES fields out of a boxed
    /// Option/Result binding — `match o { Some(Holder { name, id }) => … }`
    /// where `o`'s `Option[Holder]` payload was heap-boxed (wide). The
    /// let-site `track_boxed_enum_var` queued a `BoxedEnumDrop` whose inner
    /// `__karac_drop_struct_<T>` walk frees EVERY heap field in the box —
    /// but the destructure bit-copied the bound fields into leaf bindings
    /// that register their own cleanup, so the consumed fields double-freed
    /// (LLVM even folds the two `free`s back-to-back; probes without an
    /// OBSERVED payload were vacuously green — the malloc/free pair gets
    /// DCE'd). Zero each CONSUMED field's cap inside the box
    /// (`zero_struct_field_move_cap` — Vec/String cap, enum payload caps,
    /// nested struct recursion) so the box's walk keeps freeing only the
    /// UNBOUND fields (`Some(Holder { id, .. })` still frees `name`).
    /// Inline (≤ payload-area) struct payloads have no Option-side cleanup
    /// (no inline-struct free exists), so only the boxed width needs this.
    /// B-2026-08-28-66 — the WHOLE-PAYLOAD-BINDING sibling of
    /// [`Self::suppress_boxed_payload_struct_destructure`].
    ///
    /// That one handles `match o { Some(Holder { name, id }) => .. }`, zeroing
    /// the cap of each field the pattern BINDS so the box's inner struct walk
    /// keeps freeing only the unbound ones. `match o { Some(r) => { r } .. }`
    /// binds the payload WHOLE, which is the same transfer with every field
    /// bound at once — but it takes neither that path (the sub-pattern is a
    /// `Binding`, not a `Struct`) nor any other, so nothing was disarmed: the
    /// box's `__karac_drop_struct_<T>` freed the payload's heap fields while
    /// the value handed out of the arm was freed by whoever received it.
    ///
    /// Measured on `let o: Option[R] = Some(R { id: 1, name: f"n1" }); let k =
    /// match o { Some(r) => { r } .. };` with `impl Drop for R` — `Invalid
    /// free()`, 11 allocs / 12 frees under valgrind at -O0, and an abort under
    /// the JIT. Visible in the IR as `__karac_drop_struct_R(%o_box_ptr)`
    /// standing beside `karac_drop_R(%k)`.
    ///
    /// WHY IT LOOKED LIKE A "heap vs scalar" SPLIT and is not: `Option[R]` with
    /// `R = { i64, String }` is 4 words, wider than the `Option` inline payload
    /// area (3), so it is heap-BOXED; the `i64`-only struct fits inline and
    /// takes a different path entirely. The axis is inline-vs-boxed, and the
    /// heap field is only what makes the doubled free observable. At -O2 the
    /// optimizer erases the doubled malloc/free pair, so the default build
    /// reports nothing — which is why this survived a body-count matrix.
    ///
    /// The box MEMORY is still freed by the queued `BoxedEnumDrop`; only its
    /// inner field walk is neutralized, exactly as in the destructure sibling.
    pub(super) fn suppress_boxed_payload_whole_binding(
        &mut self,
        scrutinee: &Expr,
        pattern: &Pattern,
        arm_body: &Expr,
    ) {
        if self.pattern_state.pattern_binding_is_borrow {
            return;
        }
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return;
        };
        if !self
            .payload_vars
            .boxed_enum_payload_vars
            .contains(name.as_str())
        {
            return;
        }
        let PatternKind::TupleVariant { path, patterns } = &pattern.kind else {
            return;
        };
        let variant = path.last().map(|s| s.as_str()).unwrap_or("");
        let enum_name = match variant {
            "Some" => "Option",
            "Ok" | "Err" => "Result",
            _ => return,
        };
        // WHOLE-payload only: exactly one sub-pattern and it is a plain
        // binding. A destructure is the sibling's job, and a wildcard binds
        // nothing so the box must keep owning the fields.
        let [sub] = patterns.as_slice() else {
            return;
        };
        let PatternKind::Binding(bound) = &sub.kind else {
            return;
        };
        // THE POSITIVE SIGNAL: the arm's VALUE must be this binding, and
        // something downstream must take that value. Both halves are
        // load-bearing, and each was measured by getting it wrong first:
        //
        //   * Gating instead on "the arm does not merely borrow the payload"
        //     also fires for an arm that BINDS the payload and yields
        //     something else (`Some(r) => { R { id: 9, .. } }`). There the box
        //     must keep owning the fields, and disarming leaked them —
        //     13 allocs / 12 frees where 13 / 13 was right.
        //   * Without `branch_value_is_owned`, a DISCARDED match disarms the
        //     box while nothing downstream takes the payload, trading the
        //     double free for a leak — 11 allocs / 10 frees where 11 / 11 was
        //     right.
        //
        // Together they say exactly "this value has left the arm and someone
        // else now owns it", which is the only condition under which the
        // source's box may stop owning its fields.
        let tail_is_this_binding = matches!(
            &Self::block_tail_expr(arm_body).kind,
            ExprKind::Identifier(t) if t == bound
        );
        if !tail_is_this_binding || !self.branch_value_is_owned(scrutinee) {
            return;
        }
        let Some(struct_name) = self
            .payload_vars
            .boxed_enum_payload_struct
            .get(name.as_str())
            .cloned()
        else {
            return;
        };
        let Some(fields) = self
            .type_decls
            .struct_field_names
            .get(struct_name.as_str())
            .cloned()
        else {
            return;
        };
        let Some(&st) = self.type_decls.struct_types.get(struct_name.as_str()) else {
            return;
        };
        // Same boxing predicate as the sibling — an inline payload's w0 is not
        // a pointer, and zeroing through it would corrupt memory.
        let area = if enum_name == "Option" { 3 } else { 5 };
        if Self::llvm_type_word_count(st.into()) <= area {
            return;
        }
        let Some(layout) = self.type_decls.enum_layouts.get(enum_name) else {
            return;
        };
        let Some(slot) = self.variables.get(name.as_str()).map(|s| s.ptr) else {
            return;
        };
        let Some(fn_val) = self.current_fn else {
            return;
        };
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let Ok(w0_ptr) =
            self.builder
                .build_struct_gep(layout.llvm_type, slot, 1, "boxwhole.suppress.w0")
        else {
            return;
        };
        let w0 = self
            .builder
            .build_load(self.context.i64_type(), w0_ptr, "boxwhole.suppress.w0v")
            .unwrap()
            .into_int_value();
        let box_ptr = self
            .builder
            .build_int_to_ptr(w0, ptr_ty, "boxwhole.suppress.box")
            .unwrap();
        let is_null = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                box_ptr,
                ptr_ty.const_null(),
                "boxwhole.suppress.isnull",
            )
            .unwrap();
        let do_bb = self
            .context
            .append_basic_block(fn_val, "boxwhole.suppress.do");
        let join_bb = self
            .context
            .append_basic_block(fn_val, "boxwhole.suppress.join");
        self.builder
            .build_conditional_branch(is_null, join_bb, do_bb)
            .unwrap();
        self.builder.position_at_end(do_bb);
        for field in &fields {
            self.zero_struct_field_move_cap(box_ptr, &struct_name, field);
        }
        self.builder.build_unconditional_branch(join_bb).unwrap();
        self.builder.position_at_end(join_bb);
    }

    pub(super) fn suppress_boxed_payload_struct_destructure(
        &mut self,
        scrutinee: &Expr,
        pattern: &Pattern,
    ) {
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return;
        };
        if !self
            .payload_vars
            .boxed_enum_payload_vars
            .contains(name.as_str())
        {
            return;
        }
        let Some(slot) = self.variables.get(name.as_str()).copied() else {
            return;
        };
        self.suppress_boxed_payload_struct_destructure_at(slot.ptr, pattern);
    }

    /// B-2026-08-04-6 — the FRESH-TEMP twin of
    /// [`Self::suppress_boxed_payload_struct_destructure`]. A temp scrutinee
    /// (`match opt(i) { Some(Full { name, buf: _ }) => … }`) has no named
    /// variable to look up, so the named entry point bails on its first line
    /// and nothing disarmed anything; paired with the tracker's box-ONLY free
    /// that left every field of a destructured payload unowned, and the
    /// UNBOUND ones leaked. `track_freshtemp_boxed_enum_scrutinee` now walks
    /// the whole interior for a struct destructure and hands its staged box
    /// slot here so the BOUND fields get disarmed exactly as they do on the
    /// named path.
    pub(super) fn suppress_freshtemp_boxed_payload_struct_destructure(
        &mut self,
        slot: Option<PointerValue<'ctx>>,
        pattern: &Pattern,
    ) {
        let Some(slot) = slot else { return };
        self.suppress_boxed_payload_struct_destructure_at(slot, pattern);
    }

    /// Shared body of the two entry points above: `slot` points at the
    /// Option/Result aggregate whose w0 holds the box pointer.
    fn suppress_boxed_payload_struct_destructure_at(
        &mut self,
        slot_ptr: PointerValue<'ctx>,
        pattern: &Pattern,
    ) {
        let PatternKind::TupleVariant { path, patterns } = &pattern.kind else {
            return;
        };
        let variant = path.last().map(|s| s.as_str()).unwrap_or("");
        let enum_name = match variant {
            "Some" => "Option",
            "Ok" | "Err" => "Result",
            _ => return,
        };
        let Some(sub) = patterns.first() else {
            return;
        };
        let PatternKind::Struct {
            path: spath,
            fields,
            ..
        } = &sub.kind
        else {
            return;
        };
        let Some(struct_name) = spath.last().cloned() else {
            return;
        };
        // B-2026-08-31-23 — the payload may be a user ENUM VARIANT
        // (`Some(E.A { s })`), not only a struct (`Some(S { s })`). This
        // resolved the pattern's last path segment against `struct_types`
        // ONLY, so `E.A` looked up "A", found nothing, and returned before
        // anything was disarmed: the box's interior drop stayed armed while
        // the arm's binding owned the same buffer, and both freed it. The
        // struct payload was always clean, which is the axis — not the
        // nesting depth of the match, and not the payload's width.
        let payload = if self
            .type_decls
            .struct_types
            .contains_key(struct_name.as_str())
        {
            BoxedPayloadShape::Struct(struct_name.clone())
        } else if let Some(en) = self.variant_pattern_enum_name(sub) {
            BoxedPayloadShape::EnumVariant(en)
        } else {
            return;
        };
        // Boxed iff the payload's width exceeds the enum's inline payload
        // area — the same predicate the pack side (`coerce_to_payload_words`)
        // and the unpack side (`reconstruct_payload_value`'s debox) use. An
        // inline payload's w0 is NOT a pointer; zeroing through it would
        // corrupt memory.
        let payload_words = match &payload {
            BoxedPayloadShape::Struct(n) => {
                let Some(&st) = self.type_decls.struct_types.get(n.as_str()) else {
                    return;
                };
                Self::llvm_type_word_count(st.into())
            }
            BoxedPayloadShape::EnumVariant(n) => {
                let Some(l) = self.type_decls.enum_layouts.get(n.as_str()) else {
                    return;
                };
                if l.is_shared {
                    // A `shared enum` payload is an RC handle, not a value the
                    // box owns outright — the same carve-out
                    // `suppress_destructured_enum_payload_cleanup_at` makes.
                    return;
                }
                Self::llvm_type_word_count(l.llvm_type.into())
            }
        };
        let area = if enum_name == "Option" { 3 } else { 5 };
        if payload_words <= area {
            return;
        }
        let Some(layout) = self.type_decls.enum_layouts.get(enum_name) else {
            return;
        };
        let Some(fn_val) = self.current_fn else {
            return;
        };
        // We are INSIDE the matched arm, so the live variant is `variant`
        // and w0 (field 1) holds the box pointer. Defensive null-guard
        // mirrors the BoxedEnumDrop arm.
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let Ok(w0_ptr) =
            self.builder
                .build_struct_gep(layout.llvm_type, slot_ptr, 1, "boxfld.suppress.w0")
        else {
            return;
        };
        let w0 = self
            .builder
            .build_load(self.context.i64_type(), w0_ptr, "boxfld.suppress.w0v")
            .unwrap()
            .into_int_value();
        let box_ptr = self
            .builder
            .build_int_to_ptr(w0, ptr_ty, "boxfld.suppress.box")
            .unwrap();
        let is_null = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                box_ptr,
                ptr_ty.const_null(),
                "boxfld.suppress.isnull",
            )
            .unwrap();
        let do_bb = self
            .context
            .append_basic_block(fn_val, "boxfld.suppress.do");
        let join_bb = self
            .context
            .append_basic_block(fn_val, "boxfld.suppress.join");
        self.builder
            .build_conditional_branch(is_null, join_bb, do_bb)
            .unwrap();
        self.builder.position_at_end(do_bb);
        match &payload {
            BoxedPayloadShape::Struct(sname) => {
                for field_pat in fields {
                    // Disarm exactly the fields the pattern BINDS — those now
                    // have a leaf binding that owns and frees them. Everything
                    // else stays owned by the box: a `field: _` sub-pattern, a
                    // field omitted behind `..`, and (B-2026-08-04-6) a field
                    // the pattern only TESTS, like `Full { name: "x", buf }`.
                    // The old check was `Wildcard`-only, so a literal-test
                    // field was disarmed and then leaked — nobody owned it.
                    let binds = match &field_pat.pattern {
                        // Shorthand `Full { name }` binds by the field's own name.
                        None => true,
                        Some(sub) => Self::pattern_binds_anything(sub),
                    };
                    if !binds {
                        continue;
                    }
                    self.zero_struct_field_move_cap(box_ptr, sname, &field_pat.name);
                }
            }
            // The enum sibling reuses the NON-boxed path's own disarmer rather
            // than open-coding a second consumed-field walk: it already
            // resolves the live variant, reads `field_word_offsets` /
            // `field_drop_kinds`, and applies the same bind-vs-test rule the
            // struct arm spells out above. Only the base pointer differs —
            // the box interior here, the enum's own slot there.
            BoxedPayloadShape::EnumVariant(ename) => {
                self.suppress_destructured_enum_payload_cleanup_at(box_ptr, ename, sub);
            }
        }
        self.builder.build_unconditional_branch(join_bb).unwrap();
        self.builder.position_at_end(join_bb);
    }

    /// Disarm the scope-exit inline-payload free of a `?`-operand binding.
    /// `r?` CONSUMES `r` — on `Ok` the payload moves into the unwrap binding,
    /// on `Err` it moves into the early-returned `Err` (the caller's) — so the
    /// source's `FreeInlineResultPayload` / `FreeInlineOptionPayload` /
    /// `FreeInlineOptionMapPayload` must not fire. `compile_question` already
    /// captured the Result/Option VALUE into SSA before calling this, so the
    /// extracted/reconstructed payload keeps the live buffer; zeroing the
    /// slot's `cap` word (field 3 — Ok/Err/Some payloads overlay `{ptr,len,cap}`
    /// at w0/w1/w2) or, for `Option[Map]`, the tag (→ `None`) only neutralizes
    /// the cleanup. Without this the source frees a buffer the binding (Ok) or
    /// the caller (Err) now owns — a double-free / UAF. The Option/Result
    /// inline-payload registration (B-2026-06-10-6) made this `?`-site
    /// suppression load-bearing; before it, no inline-payload free existed.
    pub(super) fn suppress_question_source_inline_payload(&self, inner: &Expr) {
        let ExprKind::Identifier(name) = &inner.kind else {
            return;
        };
        let Some(slot) = self.variables.get(name.as_str()) else {
            return;
        };
        let i64_t = self.context.i64_type();
        if self
            .payload_vars
            .inline_result_payload_vars
            .contains(name.as_str())
        {
            if let Some(layout) = self.type_decls.enum_layouts.get("Result") {
                if let Ok(cap_ptr) = self.builder.build_struct_gep(
                    layout.llvm_type,
                    slot.ptr,
                    3,
                    "q.respl.suppress.cap",
                ) {
                    let _ = self.builder.build_store(cap_ptr, i64_t.const_int(0, false));
                }
            }
        }
        if self
            .payload_vars
            .inline_option_payload_vars
            .contains(name.as_str())
        {
            if let Some(layout) = self.type_decls.enum_layouts.get("Option") {
                if let Ok(cap_ptr) = self.builder.build_struct_gep(
                    layout.llvm_type,
                    slot.ptr,
                    3,
                    "q.optpl.suppress.cap",
                ) {
                    let _ = self.builder.build_store(cap_ptr, i64_t.const_int(0, false));
                }
            }
        }
        if self
            .payload_vars
            .inline_option_map_payload_vars
            .contains(name.as_str())
        {
            if let Some(layout) = self.type_decls.enum_layouts.get("Option") {
                let none_tag = layout.tags.get("None").copied().unwrap_or(0);
                if let Ok(tag_ptr) = self.builder.build_struct_gep(
                    layout.llvm_type,
                    slot.ptr,
                    0,
                    "q.optmap.suppress.tag",
                ) {
                    let _ = self
                        .builder
                        .build_store(tag_ptr, i64_t.const_int(none_tag, false));
                }
            }
        }
    }

    /// True when `e`'s value is a FRESH-owned enum — a variant constructor /
    /// free-fn-call result, or an `if`/`match`/block whose every leaf is one
    /// — rather than a move/alias/borrow of an existing binding. Gates the
    /// NON-`Call` let-RHS registration of inline-payload drops
    /// (`let x = if c { Some(a) } else { None };`, B-2026-06-10-6's non-Call
    /// follow-on): registering a free for a moved-in existing Option/Result
    /// binding would double-free against the source's own free, and a borrow
    /// payload aliases foreign storage. Conservative — anything not provably
    /// fresh returns `false` (no registration → still leaks, never a
    /// double-free). A bound `Identifier`/`Path` (in `self.variables`) is a
    /// move → not fresh; a `None`-like nullary constructor (NOT a variable,
    /// empty payload) is fresh-safe; `MethodCall` is excluded (matches the
    /// existing Call-only gate — `pop` rides the Vec machinery, `get`/`first`
    /// are borrows). The detectors themselves still reject borrow (`ref T`)
    /// and non-heap payloads, so this only adds the move/alias guard.
    pub(super) fn rhs_is_fresh_inline_enum(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Call { .. } => true,
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
                then_block
                    .final_expr
                    .as_deref()
                    .is_some_and(|t| self.rhs_is_fresh_inline_enum(t))
                    && else_branch
                        .as_deref()
                        .is_some_and(|t| self.rhs_is_fresh_inline_enum(t))
            }
            ExprKind::Match { arms, .. } => {
                !arms.is_empty() && arms.iter().all(|a| self.rhs_is_fresh_inline_enum(&a.body))
            }
            ExprKind::Block(b) | ExprKind::Seq(b) => b
                .final_expr
                .as_deref()
                .is_some_and(|t| self.rhs_is_fresh_inline_enum(t)),
            ExprKind::LabeledBlock { body, .. } => body
                .final_expr
                .as_deref()
                .is_some_and(|t| self.rhs_is_fresh_inline_enum(t)),
            // `None` / nullary variant constructor: not a tracked binding,
            // empty payload → fresh-safe (a taken `None` leaf frees nothing).
            // A bound identifier is a move/alias of an existing enum → NOT
            // fresh (would double-free).
            ExprKind::Identifier(n) => !self.variables.contains_key(n.as_str()),
            ExprKind::Path { segments, .. } => segments
                .last()
                .map(|s| !self.variables.contains_key(s.as_str()))
                .unwrap_or(false),
            _ => false,
        }
    }

    /// B-track (pattern-arm unbound heap-field drop, see
    /// `docs/spikes/pattern-arm-unbound-field-drop.md`): when an if-let /
    /// while-let / let-else / match scrutinee is a FRESH-OWNED enum
    /// *temporary* (a `Call` / `MethodCall` return), it has no source
    /// `EnumDrop` registered — so any heap-bearing payload field the arm
    /// leaves UNBOUND leaks (IR-proven on `main`: `if let Full(_, n) = make()`
    /// extracts the `{ptr,len,cap}` words but emits no `free`). Materialize the
    /// scrutinee value into an alloca and `track_enum_var` it, so the enum's
    /// `__karac_drop_<E>` walk frees its heap payload at scope exit. The caller
    /// then runs `suppress_destructured_enum_payload_cleanup_at(alloca,
    /// enum_name, pattern)` after binding, which zeroes the caps of fields the
    /// pattern moved into bindings — leaving only the *unbound* heap fields for
    /// the drop walk (move-out-aware partial drop). On a miss edge the caller
    /// runs no suppression, so the drop walk frees the whole temp wholesale.
    ///
    /// Gated to fresh-temp `Call` / `MethodCall` scrutinees: a *place*
    /// scrutinee (an existing binding / field) is owned elsewhere and already
    /// has its own `EnumDrop`, so minting a second would double-free.
    /// `track_enum_var` registers the drop in the *current* scope frame (the
    /// one active when the construct is compiled), so the EnumDrop fires at the
    /// enclosing scope's exit on every path. Returns `(alloca, enum_name)` for
    /// the suppression call, or `None` (no-op, prior leak behavior) when the
    /// scrutinee isn't a fresh-temp non-shared enum with a heap-bearing layout.
    pub(super) fn materialize_freshtemp_enum_scrutinee(
        &mut self,
        scrutinee: &Expr,
        pattern: &Pattern,
        val: BasicValueEnum<'ctx>,
        force: bool,
    ) -> Option<(PointerValue<'ctx>, String)> {
        // A heap `Vec`-index enum scrutinee (`match toks[i] { Word(s) => … }`,
        // the lexer's token-consume shape) is NOT a fresh-owned temp, but the
        // caller has already DEEP-CLONED `val` (see the
        // `clone_owned_vec_index_element` call at the match scrutinee compile
        // site), so the materialized temp owns an independent buffer. Drop-track
        // it here exactly like a `Call`/`MethodCall` temp: `track_enum_var` frees
        // an unbound arm's payload and the caller's per-field suppression hands a
        // bound arm's payload to the binding, while the source element stays
        // intact (matching the interpreter's read-a-copy semantics for `v[i]`).
        // Without the materialization the clone would leak on a no-bind arm.
        // `force` (the #38 borrowed-index-field clone): the caller already
        // deep-cloned `val` into an OWNED temp, so the scrutinee-shape gate
        // (which only recognizes fresh-temp calls / bare heap Vec-index) does
        // not apply — drop-track the clone regardless of the scrutinee Expr.
        let heap_index = self.expr_is_heap_vec_index(scrutinee);
        // Hoisted out of the gate below because the user-drop registration at
        // the bottom needs the same answer: only a GENUINE fresh temp is a
        // value of its own. See the B-2026-08-29-37 note there.
        let is_fresh_owned_temp = self.expr_yields_fresh_owned_temp(scrutinee);
        if !force && !is_fresh_owned_temp && !heap_index {
            return None;
        }
        let BasicValueEnum::StructValue(sv) = val else {
            return None;
        };
        let enum_name = self.variant_pattern_enum_name(pattern)?;
        let layout = self.type_decls.enum_layouts.get(&enum_name)?;
        if layout.is_shared {
            return None;
        }
        // Materialize when a variant has a heap-bearing payload to drop
        // (`track_enum_var` / `emit_enum_drop_switch`) OR the enum type carries
        // a user `impl Drop` whose body must run.
        let has_droppable = layout
            .field_drop_kinds
            .values()
            .any(|ks| ks.iter().any(|k| *k != super::state::EnumDropKind::None));
        // B-2026-07-11-26: a fresh-temp enum scrutinee whose type has a user
        // `impl Drop` must RUN that Drop (its side effects — unlock, close,
        // log) exactly as a bound `let s = <expr>` binding would. Pre-fix,
        // materialization gated on `has_droppable` alone, so an all-scalar
        // user-Drop enum returned None here and the user Drop was silently
        // SKIPPED for `if let`/`while let`/`let…else`/`match` on a fresh-temp
        // scrutinee (while a plain `let` binding of the same enum ran it).
        let has_user_drop = self
            .program_snapshot
            .as_deref()
            .map(|p| p.drop_method_keys.contains_key(&enum_name))
            .unwrap_or(false);
        if !has_droppable && !has_user_drop {
            return None;
        }
        let llvm_ty = layout.llvm_type;
        let fn_val = self.current_fn?;
        let alloca = self.create_entry_alloca(fn_val, "__freshtemp_enum_scrut", llvm_ty.into());
        let _ = self.builder.build_store(alloca, sv);
        // Register heap-field cleanup FIRST so it fires LAST (the cleanup drain
        // is LIFO), and the user-drop body SECOND so it fires FIRST — the user
        // `drop()` runs while the enum's fields are still valid, then the
        // fields are freed, matching Drop ordering. The enum user-drop wrapper
        // does NOT free enum fields itself (its field-cleanup handoff,
        // `emit_struct_drop_synthesis`, is struct-only), so the two registrations
        // don't overlap. The caller's `suppress_destructured_enum_payload_cleanup`
        // (then-arm only) still zeroes moved-in field caps for the `track_enum_var`
        // path; the user body reads fields shallowly and frees nothing.
        if has_droppable {
            self.track_enum_var(&enum_name, alloca);
        }
        // B-2026-08-29-37 — the enum's OWN `impl Drop` body belongs to the value
        // the SOURCE PROGRAM named, and a defensive copy is not one of those.
        //
        // Two disjoint populations reach the alloca above, and only the first is
        // a value in the source language:
        //
        // - A GENUINE FRESH TEMP (`match mk() { … }`) — nobody else owns it, so
        //   its body must run here or nowhere. That is B-2026-07-11-26, and it
        //   is why the registration exists at all.
        //
        // - A DEFENSIVE COPY of a place the source still owns — every `force`
        //   leg (ref-chain `<refparam>.f`, loop element, live local, borrowed
        //   index-field) plus the `heap_index` leg. These clones are emitted so
        //   a consuming arm's binding gets an INDEPENDENT buffer; the original
        //   is untouched and runs its own body at its own death. Running the
        //   clone's body too makes the count depend on a codegen internal:
        //   `fn grab(ref self) { match self.e { E.A(r) => … } }` printed `dE`
        //   inside the callee AND again at the caller's walk, where the
        //   interpreter — which never copies — printed one.
        //
        // The MEMORY half above stays registered for both: the clone really does
        // own duplicated buffers, and `__karac_drop_<E>` frees them without
        // running any user body (it recurses into `__karac_drop_struct_*`, which
        // only calls `karac_free_buf`). So the two halves separate exactly along
        // the line that matters — the copy is freed, the copy is not mourned.
        //
        // This is the ENUM-level twin of the rule `enum_payload_clone_is_faithful`
        // already enforces one level down for a `NestedStruct` PAYLOAD's body
        // (B-2026-08-09-9: "a clone is a compiler-internal artifact the source
        // language never asked for"). That guard refuses to clone; this one
        // cannot, because declining here would re-open the double-frees the clone
        // legs exist to fix (B-2026-07-21-5/-6, B-2026-07-14-1) — so it keeps the
        // clone and withholds the body instead.
        if has_user_drop && is_fresh_owned_temp {
            self.track_user_drop_var(&enum_name, "__freshtemp_enum_scrut", alloca);
        }
        Some((alloca, enum_name))
    }

    /// B-2026-08-30-15 — the STRUCT sibling of
    /// [`Self::materialize_freshtemp_enum_scrutinee`], and the last flavour of
    /// scrutinee with no owner at all.
    ///
    /// `match mkS() { S { r } => … }` over a struct with its own `impl Drop`
    /// evaluated a value nothing owned: the enum materializer bails at
    /// `variant_pattern_enum_name`, which is `None` for a struct pattern, and no
    /// sibling picked it up. So the type's `drop()` — an unlock, a close, a
    /// flush — simply never ran under `karac build`, while the interpreter ran
    /// it. Nothing was mis-ORDERED; the body was absent, which is why no
    /// ordering fix could have reached it.
    ///
    /// THE MEASUREMENT IS BROADER THAN THE ROW THAT FILED IT, and the extra
    /// half is memory, not just bodies. Against the pre-fix compiler:
    ///
    ///   match mkS() { S { r: r } => … }   interp `v7 dS dR7`  build `v7 dR7`
    ///   match mkS() { S { r: _ } => … }   interp `v  dS dR7`  build `v`
    ///   match mkS() { s        => … }     interp `v7 dS dR7`  build `v7`
    ///
    /// The two arms that move nothing out lose BOTH bodies and leak the field's
    /// buffer (15 B for a one-`String` payload, valgrind, `KARAC_OPT_LEVEL=0`) —
    /// there is no owner, so there is nothing to run a body OR a free. The row
    /// only saw the destructuring arm, where the moved-out binding happens to
    /// carry the field's body and buffer, hiding the memory half.
    ///
    /// Gated to types with their OWN `impl Drop`, deliberately. That is the
    /// defect as filed, and it keeps this off the path of the no-own-`Drop`
    /// struct whose field body the two backends already disagree about in the
    /// OPPOSITE direction (interpreter runs none, compiled runs the binding's) —
    /// B-2026-08-31-32, an interpreter-side row that must not be perturbed from
    /// here. `track_user_drop_var` registers the `karac_drop_<S>` wrapper, which
    /// is body + field cleanup + memory in ONE action, so the leak above closes
    /// with the body rather than needing a second registration.
    ///
    /// A DESTRUCTURING ARM KEEPS A KNOWN RESIDUAL, stated because this fix does
    /// not close it: when an arm moves a field out, the caller's per-arm
    /// `suppress_destructured_struct_pattern_cleanup_at` zeroes that field's cap
    /// so the wrapper's walk cannot double-FREE it — but the wrapper still runs
    /// the field's BODY on the husk, and the two legitimate bodies come out in
    /// the reverse of the interpreter's order. That is B-2026-08-31-31, which
    /// the BOUND spelling (`let s = mkS(); match s { … }`) already exhibits
    /// identically. This fix makes the fresh-temp spelling agree with the bound
    /// one; -31 then moves both at once. (The row's claim that the bound
    /// spelling "is the oracle and is clean" is not correct — measured
    /// `v7 dR7 dS dR7` against the interpreter's `v7 dS dR7`.)
    pub(super) fn materialize_freshtemp_struct_scrutinee(
        &mut self,
        scrutinee: &Expr,
        patterns: &[&Pattern],
        val: BasicValueEnum<'ctx>,
    ) -> Option<(PointerValue<'ctx>, String)> {
        if !self.expr_yields_fresh_owned_temp(scrutinee) {
            return None;
        }
        if self.scrutinee_is_borrow_call(scrutinee) {
            return None;
        }
        // A value struct arrives as a struct; a `shared` struct is the RC box
        // pointer and belongs to the RC channel, which already owns it.
        let BasicValueEnum::StructValue(sv) = val else {
            return None;
        };
        let struct_name = self.freshtemp_struct_scrutinee_type_name(scrutinee, patterns)?;
        if self.type_decls.shared_types.contains_key(&struct_name) {
            return None;
        }
        // Only the type's OWN `impl Drop` body is unowned here. A type that
        // merely CONTAINS droppable fields has no wrapper to register and is
        // left exactly as it was.
        let has_user_drop = self
            .program_snapshot
            .as_deref()
            .map(|p| p.drop_method_keys.contains_key(&struct_name))
            .unwrap_or(false);
        if !has_user_drop {
            return None;
        }
        // AN ARM THAT MOVES A FIELD OUT IS DECLINED, and this is the whole
        // reason the fix stops short of the row's own headline repro.
        //
        // `track_user_drop_var` registers `karac_drop_<S>` — user body, then
        // field bodies, then memory, as ONE action. When an arm binds a field,
        // that field's value and its `Drop` belong to the binding, so the
        // wrapper's field-body step runs the SAME body a second time. Measured:
        // registering it anyway turns `match mkS() { S { r } => … }` from
        // `v7 dR7` into `v7 dR7 dS dR7` — the missing `dS` appears, and `dR7`
        // now runs TWICE. For a `drop()` that closes a handle that is a double
        // close, so it is a REGRESSION on a shape that is correct today, not a
        // partial fix. The codebase's own bar says so in as many words: "an
        // under-suppressed field drop double-frees; an over-suppressed one
        // leaks."
        //
        // The caller's `suppress_destructured_struct_pattern_cleanup_at` cannot
        // rescue it: that zeroes the moved field's CAP, which stops the second
        // FREE and not the second BODY. Masking the body needs a per-variable
        // wrapper variant that does not exist — B-2026-08-31-31, which the BOUND
        // spelling (`let s = mkS(); match s { … }`) exhibits identically, at
        // `v7 dR7 dS dR7` against the interpreter's `v7 dS dR7`. When -31 lands,
        // dropping this gate is the follow-up.
        //
        // Everything that moves nothing out — a wildcard field, a bare binding
        // of the whole value, an all-scalar struct — has no competing owner and
        // reaches full interpreter parity, which is what this fix delivers.
        if patterns
            .iter()
            .any(|p| self.struct_pattern_binds_a_drop_bearing_field(p, &struct_name))
        {
            return None;
        }
        let st = *self.type_decls.struct_types.get(&struct_name)?;
        let fn_val = self.current_fn?;
        let alloca = self.create_entry_alloca(fn_val, "__freshtemp_struct_scrut", st.into());
        let _ = self.builder.build_store(alloca, sv);
        self.track_user_drop_var(&struct_name, "__freshtemp_struct_scrut", alloca);
        Some((alloca, struct_name))
    }

    /// The user-struct type name a fresh-temp scrutinee denotes, for
    /// [`Self::materialize_freshtemp_struct_scrutinee`].
    ///
    /// Two sources, pattern first: a `S { … }` arm names the type directly, and
    /// `struct_pattern_names_a_user_struct` is the existing predicate that tells
    /// a real struct pattern from an enum STRUCT-VARIANT pattern spelled the
    /// same way (the scrutinee-hint rule). A bare-binding or wildcard arm names
    /// nothing, so fall back to the callee's declared return type — the shape
    /// `fresh_arg_bare_shared_heap_type` already reads for its own answer.
    ///
    /// Returns `None` for a name that is an ENUM (a struct-variant pattern) or
    /// is not a known struct, so the enum path keeps every case it had.
    fn freshtemp_struct_scrutinee_type_name(
        &self,
        scrutinee: &Expr,
        patterns: &[&Pattern],
    ) -> Option<String> {
        let from_pattern = patterns.iter().find_map(|p| match &p.kind {
            PatternKind::Struct { path, .. } if self.struct_pattern_names_a_user_struct(p) => {
                path.last().cloned()
            }
            _ => None,
        });
        let name = match from_pattern {
            Some(n) => n,
            None => match &scrutinee.kind {
                ExprKind::Call { callee, .. } => match &callee.kind {
                    ExprKind::Identifier(n) => self.fn_sig.fn_return_type_names.get(n).cloned()?,
                    _ => return None,
                },
                _ => return None,
            },
        };
        if self.type_decls.enum_layouts.contains_key(&name) {
            return None;
        }
        if !self.type_decls.struct_types.contains_key(&name) {
            return None;
        }
        Some(name)
    }

    /// Does this pattern move a field that CARRIES A `Drop` BODY out of the
    /// matched struct? The decline condition for
    /// [`Self::materialize_freshtemp_struct_scrutinee`].
    ///
    /// Two narrowings, and both are load-bearing:
    ///
    /// - Narrower than [`Self::pattern_binds_anything`], which is also true of a
    ///   bare `s =>` catch-all. That case binds the WHOLE value and takes
    ///   nothing out of it, so no other owner competes and it is exactly the
    ///   case the caller wants to keep.
    /// - Narrower than "binds any field", because only a field whose type runs a
    ///   body can be double-run by the wrapper's field step. `S { n }` over an
    ///   `i64` field moves nothing droppable; declining it would lose the
    ///   struct's own `dS` for no reason (measured: it did, until this was
    ///   narrowed). The field's MEMORY needs no protection here — the caller's
    ///   `suppress_destructured_struct_pattern_cleanup_at` zeroes the moved
    ///   field's cap, so a `String`/`Vec` field is already safe from a double
    ///   free; it is the BODY the mask cannot reach.
    fn struct_pattern_binds_a_drop_bearing_field(&self, pat: &Pattern, struct_name: &str) -> bool {
        match &pat.kind {
            PatternKind::Struct { fields, .. } => {
                let Some(field_names) = self.type_decls.struct_field_names.get(struct_name) else {
                    // Unknown layout — assume the worst and decline.
                    return true;
                };
                let Some(field_tys) = self.type_decls.struct_field_type_names.get(struct_name)
                else {
                    return true;
                };
                fields.iter().any(|f| {
                    let binds = match &f.pattern {
                        Some(sub) => Self::pattern_binds_anything(sub),
                        None => true,
                    };
                    if !binds {
                        return false;
                    }
                    match field_names.iter().position(|n| n == &f.name) {
                        Some(idx) => field_tys
                            .get(idx)
                            .and_then(|o| o.as_deref())
                            .is_some_and(|tn| self.type_runs_user_drop(tn, &mut Vec::new())),
                        // A field the layout does not name — decline rather than
                        // guess which one moved.
                        None => true,
                    }
                })
            }
            PatternKind::Or(alts) => alts
                .iter()
                .any(|a| self.struct_pattern_binds_a_drop_bearing_field(a, struct_name)),
            _ => false,
        }
    }

    /// Oversized-enum-payload follow-up §1/§2
    /// ([`docs/spikes/oversized-enum-payload.md`]): a fresh-temp scrutinee
    /// (`match v.pop() { … }`, `if let Some(e) = v.pop()`) whose payload `T`
    /// was heap-boxed because its LLVM word count exceeds the seeded area
    /// (Option = 3, Result = 5 — see `coerce_to_payload_words`) has no named
    /// binding, so the let-site `track_boxed_enum_var` never queues the box
    /// free → the box leaks (invisible on macOS: no LeakSanitizer). When an
    /// arm binds the boxed variant's payload we recover `T`'s width from the
    /// pattern (mirroring `reconstruct_payload_value`'s `want > area` unbox
    /// predicate); materialize the Option/Result struct into an alloca and
    /// queue a `BoxedEnumDrop` for the box.
    ///
    /// **Box-only free** (`inner_struct_name = None`): the bound payload now
    /// owns `T`'s inner heap and frees it through its own binding cleanup, so
    /// re-dropping `T` here would double-free (the §2 move-out interaction).
    /// Freeing just the box is sound for both the all-inline payload (the §1
    /// `Entity` repro — no inner heap to leak) and the heap-owning bound
    /// payload. The narrow remaining leak — an *unbound* heap-owning boxed
    /// payload (`Some(_)` where `T` owns heap) — needs the scrutinee's static
    /// type, which a wildcard pattern doesn't carry; deferred (spike §1).
    ///
    /// Gated to fresh `Call` / `MethodCall` scrutinees so a *place* scrutinee
    /// (owned elsewhere, with its own let-site box drop) is untouched.
    /// Registers in the *current* scope frame, matching
    /// `materialize_freshtemp_enum_scrutinee`'s per-construct framing (enclosing
    /// frame for match/if-let/let-else, per-iteration body frame for while-let).
    /// No-op (the prior leak behavior) for non-fresh / non-Option-Result /
    /// fitting-payload scrutinees.
    /// For a `vec.pop()` / `pop_back()` / `pop_front()` scrutinee over a
    /// `Vec[Option[shared T]]`, return the boxed-payload inner drop fn — the
    /// `Option[T]` element drop that null-guards and rc-decs the popped node.
    /// `None` for any other scrutinee shape (non-pop call, non-Identifier vec
    /// object, element not `Option[shared]`), so the caller falls back to the
    /// user-struct-name box-drop derivation. The element `TypeExpr` comes from
    /// `var_elem_type_exprs`, the same table the pop-result-typing path uses.
    /// B-2026-07-12-4 pop-consume half.
    fn nested_option_shared_pop_inner_drop(
        &mut self,
        scrutinee: &Expr,
    ) -> Option<inkwell::values::FunctionValue<'ctx>> {
        let ExprKind::MethodCall { object, method, .. } = &scrutinee.kind else {
            return None;
        };
        if !matches!(method.as_str(), "pop" | "pop_back" | "pop_front") {
            return None;
        }
        let ExprKind::Identifier(vec_name) = &object.kind else {
            return None;
        };
        let elem_te = self
            .var_types
            .var_elem_type_exprs
            .get(vec_name.as_str())?
            .clone();
        // The popped element (`Option[shared T]`) IS the box's boxed payload;
        // synthesize its element drop directly.
        self.option_shared_payload_element_drop(&elem_te)
    }

    /// Given a boxed-payload `TypeExpr` that is itself `Option[shared T]`,
    /// return the `Option[T]` element drop fn (`karac_drop_Option_<T>`, a
    /// null-guarded rc-dec of the node). `None` when `payload_te` is not
    /// `Option[shared]`. Shared by the fresh-temp pop scrutinee path and the
    /// let-binding box-drop path (B-2026-07-12-4).
    pub(super) fn option_shared_payload_element_drop(
        &mut self,
        payload_te: &TypeExpr,
    ) -> Option<inkwell::values::FunctionValue<'ctx>> {
        self.option_inner_shared_type_for_type_expr(payload_te)?;
        let inner_te = Self::option_generic_arg_type_expr(payload_te)?;
        self.emit_option_drop_fn(&inner_te)
    }

    /// Extract the `T` `TypeExpr` from an `Option[T]` `TypeExpr`, or `None` if
    /// `te` is not a single-arg `Option[...]`.
    pub(super) fn option_generic_arg_type_expr(te: &TypeExpr) -> Option<TypeExpr> {
        let TypeKind::Path(p) = &te.kind else {
            return None;
        };
        if p.segments.last().map(|s| s.as_str()) != Some("Option") {
            return None;
        }
        p.generic_args.as_ref()?.iter().find_map(|a| match a {
            crate::ast::GenericArg::Type(t) => Some(t.clone()),
            _ => None,
        })
    }

    ///
    /// Returns the staged `__freshtemp_boxed_scrut` alloca when it registered
    /// one. B-2026-08-04-1 needs it: that slot — not the arm binding's
    /// reconstructed copy — is the subject the payload's Drop BODY walk must
    /// run against, because the box it points at is what the scope-exit box
    /// drop later reads. See `freshtemp_payload_bodies_action`.
    /// B-2026-08-18-8 — a `match` whose SCRUTINEE is itself a `match` yielding
    /// a boxed `Option`/`Result` payload.
    ///
    /// `match (match u.address { Some(a) => { a.city } None => { None } }) { … }`
    /// leaked the 32-byte payload envelope once per evaluation — unbounded in a
    /// loop — while the two-statement spelling of the identical program
    /// (`let c = match …; match c { … }`) was always clean. That gap is the
    /// whole shape of the bug: the `let` registers a `BoxedEnumDrop` for the
    /// binding, and nothing registered one for the value in scrutinee position,
    /// so the box had no owner at all. `?.` lowers to exactly this `match`, so a
    /// chain used directly as a scrutinee inherited the leak — but the defect is
    /// not `?.`'s and reproduces with none in the program.
    ///
    /// Admitting the shape here rather than widening
    /// `expr_yields_fresh_owned_temp` is deliberate: that predicate has ~50
    /// callers across codegen, each with its own aliasing contract, and this
    /// row is one position. The general case is the still-open slice 4 of
    /// [`docs/spikes/general-owned-temp-tracking.md`] (scrutinee sub-frame).
    ///
    /// FAIL-CLOSED on the arms, on the same grounds as
    /// `discarded_match_value_tail`: exactly one arm runs and the scrutinee is
    /// the merged phi, so a single arm that hands back storage someone else
    /// still owns would make the registered free a double free. Each arm's tail
    /// must be one of
    ///   * `None` — no payload, hence no box; the drop's tag guard skips it;
    ///   * a non-borrow `Call`/`MethodCall` — freshly allocated, including the
    ///     `Some(x)` variant construction whose boxing allocated the envelope;
    ///   * a FIELD or tuple-index projection, when the inner match consumed an
    ///     OWNED scrutinee and the chain's root is not a `ref` parameter — the
    ///     projection MOVES the field out and the arm's own disarm zeroes the
    ///     source, so the box changes hands;
    ///   * an identifier naming a LOCAL that owns a box — handing it out of the
    ///     arm is a move the arm-tail disarm already performs, in that arm's
    ///     own block, so the sibling arm keeps its binding armed and the tag
    ///     guard here covers the difference (B-2026-08-18-10);
    ///   * a `match` / `?.` yielding the same, recursively — a third level of
    ///     nesting reaches this frame's phi unchanged, and nothing registers
    ///     for a value in ARM-TAIL position, so one registration still owns it.
    ///
    /// AN INDEX LINK ANYWHERE IN A CHAIN DECLINES, and that one IS measured
    /// rather than cautious: `v[k].city` is an element COPY, not a move, so the
    /// container keeps the box. The `let`-bound spelling of exactly that shape
    /// SEGVs under ASAN today (filed separately) — it is the only pairing in
    /// this family where the `let` form is the broken one, which is precisely
    /// why this position must not copy what `let` does there.
    ///
    /// The field form was ALSO restricted to a chain rooted at the arm's own
    /// payload binding when B-2026-08-18-8 landed, described there as
    /// load-bearing. It was not: measurement (`0 => { other.city }`, source read
    /// again after the match) shows the `let` twin frees exactly once and the
    /// scrutinee form leaks, i.e. the same defect, so the restriction only
    /// narrowed the fix. Relaxed with B-2026-08-18-10; the `ref`-root guard is
    /// what actually matters, since there the payload aliases storage the
    /// caller keeps.
    ///
    /// Anything else declines the whole match.
    fn match_result_scrutinee_owns_box(&self, scrutinee: &Expr) -> bool {
        match &scrutinee.kind {
            ExprKind::Match {
                scrutinee: inner,
                arms,
            } => {
                if arms.is_empty() {
                    return false;
                }
                let inner_is_owned = self.optres_producer_source_is_owned(inner);
                arms.iter().all(|arm| {
                    let tail = Self::block_tail_expr(&arm.body);
                    match &tail.kind {
                        ExprKind::Identifier(n) if n == "None" => true,
                        // B-2026-08-18-10 — an arm handing out a LOCAL that owns
                        // a box. The hand-out is already a move: the arm-tail
                        // disarm (B-2026-08-07-1) zeroes the source's payload
                        // word in that arm's own block, so after it runs nothing
                        // owns the box and the phi is its only possible owner.
                        // That per-arm placement is also what makes the pair
                        // safe — the sibling arm that does NOT hand the binding
                        // out leaves it armed, and this registration's tag guard
                        // sees `None` there, so exactly one free happens on
                        // either path.
                        ExprKind::Identifier(n) => {
                            self.payload_vars
                                .boxed_enum_payload_vars
                                .contains(n.as_str())
                                && self.variables.contains_key(n.as_str())
                                && !self.borrow_vars.ref_params.contains_key(n.as_str())
                        }
                        // An arm tail that is ITSELF one of these — a third
                        // level of nesting (`match (match k { 0 => (match ...),
                        // _ => None }) { ... }`). Whatever the inner one yields
                        // reaches this frame's phi unchanged, and nothing
                        // registers for a value in ARM-TAIL position, so the
                        // same single registration at the outer scrutinee owns
                        // it. Recursion is structural and terminates.
                        ExprKind::Match { .. } | ExprKind::OptionalChain { .. } => {
                            self.match_result_scrutinee_owns_box(tail)
                        }
                        ExprKind::FieldAccess { .. } | ExprKind::TupleIndex { .. } => {
                            inner_is_owned
                                && Self::moved_field_chain_root(tail).is_some_and(|root| {
                                    !self.borrow_vars.ref_params.contains_key(root)
                                })
                        }
                        _ => self.expr_yields_fresh_owned_temp(tail),
                    }
                })
            }
            // `?.` IS the match above — `compile_optional_chain` synthesizes it
            // rather than hand-rolling a second lowering (B-2026-08-17-28), but
            // it does so from THIS node, so the arm walk never sees it. The
            // synthesized arms are fixed and need no walk: `Some(x) => x.<member>`
            // (or `Some(x.<member>)` when the chain does not flatten) and
            // `None => None`. Both are exactly what the `Match` arm above
            // admits, under the same owned-source condition.
            //
            // A CALL chain (`a?.f()`) declines: its arm is a method call whose
            // result may alias the receiver's storage, and this position cannot
            // tell a borrow-returning member from an allocating one. Declining
            // leaves it as it was — a leak, not a double free.
            ExprKind::OptionalChain { object, args, .. } => {
                args.is_none() && self.optres_producer_source_is_owned(object)
            }
            _ => false,
        }
    }

    /// Does the value an `Option`-producing chain reads FROM belong to this
    /// frame? A borrow-returning call, a borrowed binding and a `ref` parameter
    /// all say no: there the projected payload aliases storage the source keeps,
    /// and registering a free against it would double-free rather than close a
    /// leak. A nested `?.` (`a?.b?.c`) resolves through to its own root, since
    /// each level projects out of whatever the level below produced.
    /// The local a FIELD/tuple-index chain projects out of, or `None` if the
    /// chain passes through anything else. Deliberately narrower than
    /// `place_root_ident`: an `Index` link makes the leaf an element COPY
    /// rather than a move, so a chain containing one has no single owner to
    /// hand the box over to and must not be walked through here.
    fn moved_field_chain_root(expr: &Expr) -> Option<&str> {
        match &expr.kind {
            ExprKind::Identifier(name) => Some(name.as_str()),
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                Self::moved_field_chain_root(object)
            }
            _ => None,
        }
    }

    fn optres_producer_source_is_owned(&self, src: &Expr) -> bool {
        if let ExprKind::OptionalChain { object, .. } = &src.kind {
            return self.optres_producer_source_is_owned(object);
        }
        !self.scrutinee_is_borrow_call(src)
            && !self.scrutinee_is_borrowed_binding(src)
            && !matches!(&src.kind, ExprKind::Identifier(n)
                if self.borrow_vars.ref_params.contains_key(n.as_str()))
    }

    pub(super) fn track_freshtemp_boxed_enum_scrutinee(
        &mut self,
        scrutinee: &Expr,
        patterns: &[&Pattern],
        val: BasicValueEnum<'ctx>,
    ) -> Option<PointerValue<'ctx>> {
        if !self.expr_yields_fresh_owned_temp(scrutinee)
            && !self.match_result_scrutinee_owns_box(scrutinee)
        {
            return None;
        }
        let BasicValueEnum::StructValue(sv) = val else {
            return None;
        };
        for pat in patterns {
            let PatternKind::TupleVariant {
                path,
                patterns: subs,
            } = &pat.kind
            else {
                continue;
            };
            let Some(enum_name) = self.variant_pattern_enum_name(pat) else {
                continue;
            };
            let area = match enum_name.as_str() {
                "Option" => 3usize,
                "Result" => 5usize,
                _ => continue,
            };
            let Some(payload) = subs.first() else {
                continue;
            };
            // Hoisted above the width gate: B-2026-08-04-3's wildcard sizing
            // needs the variant to pick the right `Result` half.
            let Some(variant) = path.last().cloned() else {
                continue;
            };
            // B-2026-08-04-3 — a WILDCARD payload has no type of its own, so
            // `pattern_payload_word_count` falls to its 1-word default and this
            // width gate rejected the arm before the `inner_struct_name`
            // derivation below could see it. Size it from the SCRUTINEE's
            // instantiation instead, the same source that arm uses.
            let payload_words = match &payload.kind {
                PatternKind::Wildcard => self
                    .optres_scrutinee_payload_struct_name_for(scrutinee, &variant)
                    .and_then(|n| self.type_decls.struct_types.get(n.as_str()).copied())
                    .map(|st| Self::llvm_type_word_count(st.into()))
                    .unwrap_or(1),
                _ => self.pattern_payload_word_count(payload),
            };
            if payload_words <= area {
                continue;
            }
            let llvm_ty = match self.type_decls.enum_layouts.get(enum_name.as_str()) {
                Some(l) => l.llvm_type,
                None => continue,
            };
            let fn_val = self.current_fn?;
            let alloca =
                self.create_entry_alloca(fn_val, "__freshtemp_boxed_scrut", llvm_ty.into());
            let _ = self.builder.build_store(alloca, sv);
            // Inner-struct drop: when the boxed payload is bound WHOLE to a
            // non-shared user struct that owns heap (`Some(h)` where `h: H`,
            // `H` has a `Vec`/`String` field), the box drop must free that
            // inner heap too — the bound `h` is an unboxed COPY that aliases
            // the box's inner buffers but registers no struct drop of its own
            // (match-bound structs are tracked only for the seeded HTTP types,
            // pattern_binding.rs), so without this the inner `Vec` buffer leaks
            // once per call (B-2026-06-12-6 cluster 4,
            // `freshtemp_boxed_option_match_move_out`; the box itself was freed
            // fine — a no-heap boxed struct is clean). Box-only (`None`) stays
            // for a struct-DESTRUCTURE payload (`Some(H { v, .. })`) whose
            // fields are individually bound + tracked, and for non-heap / shared
            // payloads (`emit_struct_drop_synthesis` returns `None`). Mirrors
            // the named-let box drop, which carries the inner struct name from
            // the typed binding.
            // Slice 3r leg 2: a BORROW-returning scrutinee (`m.get(k)` on a
            // Map) hands back a bit-copy whose interior heap ALIASES the
            // bucket's stored value — only the box allocation itself is fresh
            // (built by `coerce_to_payload_words`' boxing path per call).
            // Running the inner struct walk here freed the map's own value
            // content: the first `get` silently disarmed the bucket, the
            // second double-freed (exit 133). Box-only free for borrow
            // scrutinees; the map's own cleanup owns the interior.
            let scrutinee_is_borrow = self.scrutinee_is_borrow_call(scrutinee);
            // Nested `Option[shared T]` boxed payload (B-2026-07-12-4): when the
            // scrutinee is `vec.pop()` over a `Vec[Option[shared T]]`, the result
            // is `Option[Option[shared T]]` and the boxed payload is itself an
            // `Option[shared T]`. The box-free must run that inner option's
            // element drop (null-guarded rc-dec of the node) or the popped node
            // leaks — the pop-consume half of B-2026-07-12-4 (a fresh
            // `Some(Node)` push drained the same way leaked too; the field-read
            // push additionally UAF'd on the residual path, fixed by the paired
            // push-side `share_option_shared_field_ref_for_arg` retain). Only
            // fires for an owned (non-borrow) pop-family scrutinee; a borrow
            // scrutinee's payload aliases the container's storage and must not be
            // dec'd here.
            let nested_opt_inner_drop = if scrutinee_is_borrow {
                None
            } else {
                self.nested_option_shared_pop_inner_drop(scrutinee)
            };
            if let Some(inner_drop) = nested_opt_inner_drop {
                self.track_boxed_enum_var_with_inner_drop(
                    &enum_name,
                    alloca,
                    &enum_name,
                    &variant,
                    Some(inner_drop),
                );
                return Some(alloca);
            }
            // Whole-TUPLE payload binding (`Some(kv)` where `kv: (String,
            // String)`, B-2026-07-18-3): the box holds a >3-word tuple whose
            // String/Vec fields leak under the box-only free — the bound `kv`
            // is an unboxed COPY that aliases the box's inner buffers and
            // registers no drop of its own (the same shape as the whole-STRUCT
            // `inner_struct_name` case below, but a tuple has no type name, so
            // it needs a synthesized per-element drop fn instead). Route the box
            // drop through `synthesize_tuple_drop_fn_te`, mirroring
            // `clone_and_track_borrow_binding`'s tuple branch. Sound with no
            // double-free: box-only-free never touches the inner buffers, and a
            // whole-tuple match binding is not otherwise drop-registered (unlike
            // a per-element destructure `Some((a, b))`, whose leaf bindings each
            // own their field — that path stays box-only via the `_ => None`
            // arm below). Excludes borrow scrutinees (their payload aliases the
            // container's storage) and the shared/nested-Option cases handled
            // above.
            if let PatternKind::Binding(_) = &payload.kind {
                if !scrutinee_is_borrow {
                    let pkey = (payload.span.offset, payload.span.length);
                    if matches!(
                        self.pattern_state
                            .pattern_binding_types
                            .get(&pkey)
                            .map(|s| s.as_str()),
                        Some("Tuple")
                    ) {
                        if let Some(te) = self
                            .pattern_state
                            .pattern_binding_inner_types
                            .get(&pkey)
                            .cloned()
                        {
                            if let TypeKind::Tuple(elem_tes) = &te.kind {
                                if let BasicTypeEnum::StructType(agg_ty) =
                                    self.llvm_type_for_type_expr(&te)
                                {
                                    let elem_tes = elem_tes.clone();
                                    let inner_drop =
                                        self.synthesize_tuple_drop_fn_te(agg_ty, &elem_tes);
                                    self.track_boxed_enum_var_with_inner_drop(
                                        &enum_name, alloca, &enum_name, &variant, inner_drop,
                                    );
                                    return Some(alloca);
                                }
                            }
                        }
                    }
                }
            }
            let inner_struct_name: Option<String> = match &payload.kind {
                PatternKind::Binding(_) if !scrutinee_is_borrow => {
                    let pkey = (payload.span.offset, payload.span.length);
                    self.pattern_state
                        .pattern_binding_types
                        .get(&pkey)
                        .cloned()
                        .filter(|n| {
                            self.type_decls.struct_types.contains_key(n)
                                && !self.type_decls.shared_types.contains_key(n)
                        })
                }
                // B-2026-08-04-3 — a WILDCARD payload binds NOTHING, so the box
                // is the only owner its interior will ever have and the walk
                // must run. Before this the arm fell into `_ => None` and the
                // free went box-ONLY, stranding the payload struct's heap
                // fields (32 bytes per `match mk() { Some(_) => … }`, probes
                // b228/x1 for `Option` and b228/x6 for `Result`'s Err side).
                //
                // Sharing the `_ => None` arm with a struct DESTRUCTURE is what
                // made this look consistent: there the arm is right, because
                // the pattern's leaf bindings each own and free their field, so
                // walking the interior would double-free. Binds-nothing and
                // binds-the-parts need opposite answers, and matching on
                // `payload.kind` alone collapsed them.
                //
                // The type comes from the SCRUTINEE's instantiation rather than
                // the sub-pattern, since a wildcard carries none — the same
                // resolution `freshtemp_payload_bodies_action` uses. Borrow
                // scrutinees stay excluded for the reason the `Binding` arm
                // gives: their box interior aliases the container's storage.
                PatternKind::Wildcard if !scrutinee_is_borrow => self
                    .optres_scrutinee_payload_struct_name_for(scrutinee, &variant)
                    .filter(|n| {
                        self.type_decls.struct_types.contains_key(n)
                            && !self.type_decls.shared_types.contains_key(n)
                    }),
                // B-2026-08-04-6 — a struct DESTRUCTURE binds SOME fields and
                // leaves the rest to nobody. The old `_ => None` arm sent the
                // whole shape to a box-only free, so `Some(Full { name, buf: _
                // })` stranded `buf`'s buffer (and `Full { name, .. }`, and
                // the `Result` Err twin). Walking the whole interior instead
                // would double-free the BOUND fields — so this arm is only
                // half the answer: the arm loop pairs it with
                // `suppress_freshtemp_boxed_payload_struct_destructure`, which
                // cap-zeroes exactly the bound fields inside the box before
                // the walk can reach them. Net effect per field: bound => the
                // leaf binding frees it, unbound => the box does. That is the
                // same split the NAMED scrutinee has always used; this arm and
                // its suppressor are that path ported to the fresh temp, whose
                // suppressor entry point requires an `Identifier` scrutinee
                // and so had been declining silently.
                //
                // All-fields-bound degenerates safely: every cap is zeroed, so
                // the walk is a no-op and the behavior matches the old
                // box-only free exactly.
                PatternKind::Struct { path, .. } if !scrutinee_is_borrow => {
                    path.last().cloned().filter(|n| {
                        self.type_decls.struct_types.contains_key(n)
                            && !self.type_decls.shared_types.contains_key(n)
                    })
                }
                _ => None,
            };
            self.track_boxed_enum_var(
                &enum_name,
                alloca,
                &enum_name,
                &variant,
                inner_struct_name.as_deref(),
            );
            return Some(alloca);
        }
        None
    }

    /// Fresh-temp INLINE (fitting, `<=` area) heap `Result` match scrutinee
    /// (`match cell.set(v) { Err(_) => {} }` — B-2026-07-12-2 gap 2a): the
    /// seeded `Result` layout has all-`None` drop kinds so
    /// `materialize_freshtemp_enum_scrutinee` never tracks it, and
    /// `track_freshtemp_boxed_enum_scrutinee` covers only a heap-BOXED WIDE
    /// payload — so a DISCARDED inline heap payload (a `String`/`Vec`, or a
    /// transparent single-field wrapper like `AlreadySetError[String]`, that
    /// fits the 5-word `Result` area) leaks. Materialize the scrutinee value
    /// into an alloca and register `FreeInlineResultPayload`: a no-bind arm
    /// (`Err(_)`) frees it, and a CONSUMING arm's per-arm
    /// `suppress_inline_result_payload_cleanup_at` zeros the alloca `cap` so the
    /// binding / consumer owns it (no double-free — the alloca shares the fresh
    /// temp's buffer, and the temp is dead after the match). Returns the alloca
    /// for the arm loop's suppression, or `None` when it does not apply
    /// (non-fresh-temp, borrow, scalar payload, or a WIDE payload — boxed path).
    /// Fresh-temp `Option[shared T]` scrutinee whose `Some` payload is a
    /// SHARED HANDLE (an rc pointer in the first payload word):
    /// `match stack.pop() { Some(popped) => … }` (B-2026-07-15-1). `Vec.pop`
    /// TRANSFERS the vec's +1 ref into the returned Option temp, and the
    /// payload-binding path takes its OWN +1 (balanced by the arm-exit
    /// `RcDec`) — so without a release of the temp's transferred ref, every
    /// popped shared element strands one count (the iterative
    /// `node = stack.pop()` tree-builder leaked its whole tree; the minimal
    /// repro leaks the popped node even with a plain
    /// `Some(p) => println(p.val)` arm). Materialize the scrutinee value
    /// into a slot and queue a tag-guarded `RcDecOption` at scope exit.
    /// Count-based, so it is correct regardless of whether an arm binds the
    /// payload, wildcards it, or moves it onward — each of those paths
    /// manages its own +1 independently. The `None` arm is covered by the
    /// action's tag guard. Boxed (>3-word) payloads take the boxed-enum
    /// tracker instead (a shared handle is one word, so the two are
    /// mutually exclusive); borrow-returning scrutinees (`Map.get`) never
    /// owned a transferable ref and are excluded by the borrow gate.
    /// B-2026-08-28-74 — the BARE `shared` enum sibling of
    /// [`Self::track_freshtemp_shared_option_scrutinee`].
    ///
    /// `match mk() { … }` over a `shared enum` evaluates the producer's `+1`
    /// into a temporary no binding names, and nothing ever releases it. The
    /// two paths that would have owned it both decline:
    /// `materialize_freshtemp_enum_scrutinee` (the fresh-temp owner-minting
    /// path for VALUE enums) fails its `BasicValueEnum::StructValue` gate,
    /// because a shared enum's value is the RC box POINTER — and its
    /// `layout.is_shared` bail sits below that anyway — while the
    /// `Option[shared T]` tracker next to this one only recognizes an `Option`
    /// wrapper. So the box, and every heap field it owns, leaks.
    ///
    /// The BOUND spelling is the oracle, and it is clean on every payload shape
    /// measured — scalar, `String`, `Vec`, shared child, recursive enum:
    /// `let t = mk(); match t { … }` registers exactly one `RcDec` through
    /// `track_rc_var` at the let site. This registers that same action for the
    /// unnamed temp, so the two spellings free identically.
    ///
    /// The rest of the machinery is already in place, which is why only the
    /// registration was missing:
    ///
    ///  * `track_rc_var` lazily synthesizes the recursive drop fn, and for a
    ///    shared ENUM `emit_shared_struct_rc_drop_fn`'s `info.is_enum` arm
    ///    routes to `emit_shared_enum_rc_drop_fn` — the tag-switched walk that
    ///    frees an UNBOUND payload's heap before the box.
    ///  * An arm that MOVES a payload out is covered by
    ///    `suppress_shared_enum_payload_move_out`, which already fires per-arm
    ///    for ANY pointer-valued scrutinee, independent of this registration:
    ///    it zeroes the consumed field's words in the box so the walk skips
    ///    what the binding now owns rather than double-freeing it.
    ///
    /// No-op for a PLACE scrutinee (an existing binding / field): that value is
    /// owned elsewhere and carries its own dec, so a second one would release
    /// it early. Same freshness gate as every sibling tracker here.
    ///
    /// B-2026-08-30-16 — returns the box pointer it registered, so a `match`
    /// can release it at the construct's exit
    /// ([`Self::fire_freshtemp_shared_scrutinee_dec_at_exit`]) instead of at the
    /// enclosing scope's. `None` on every decline path, which is what keeps the
    /// caller from firing a dec it never registered.
    pub(super) fn track_freshtemp_shared_enum_scrutinee(
        &mut self,
        scrutinee: &Expr,
        patterns: &[&Pattern],
        val: BasicValueEnum<'ctx>,
    ) -> Option<PointerValue<'ctx>> {
        if !self.expr_yields_fresh_owned_temp(scrutinee) {
            return None;
        }
        if self.scrutinee_is_borrow_call(scrutinee) {
            return None;
        }
        // A shared enum arrives as the RC box pointer; a value enum arrives as
        // a struct and belongs to `materialize_freshtemp_enum_scrutinee`.
        let BasicValueEnum::PointerValue(ptr) = val else {
            return None;
        };
        let enum_name = patterns
            .iter()
            .find_map(|p| self.variant_pattern_enum_name(p))?;
        if !self
            .type_decls
            .enum_layouts
            .get(&enum_name)
            .is_some_and(|l| l.is_shared)
        {
            return None;
        }
        let heap_type = self
            .type_decls
            .shared_types
            .get(&enum_name)
            .map(|i| i.heap_type)?;
        self.track_rc_var("__freshtemp_shared_enum", ptr, heap_type);
        Some(ptr)
    }

    pub(super) fn track_freshtemp_shared_option_scrutinee(
        &mut self,
        scrutinee: &Expr,
        patterns: &[&Pattern],
        val: BasicValueEnum<'ctx>,
    ) {
        if !self.expr_yields_fresh_owned_temp(scrutinee) {
            return;
        }
        if self.scrutinee_is_borrow_call(scrutinee) {
            return;
        }
        let BasicValueEnum::StructValue(sv) = val else {
            return;
        };
        // Resolve the shared payload's heap type from any arm's
        // `Some(<binding>)` sub-pattern (its span is in
        // `pattern_binding_types`); bail when no arm proves a shared payload.
        let mut heap_type = None;
        for pat in patterns {
            if self.variant_pattern_enum_name(pat).as_deref() != Some("Option") {
                continue;
            }
            let PatternKind::TupleVariant { patterns, .. } = &pat.kind else {
                continue;
            };
            let Some(sub) = patterns.first() else {
                continue;
            };
            let key = (sub.span.offset, sub.span.length);
            if let Some(tn) = self.pattern_state.pattern_binding_types.get(&key) {
                if let Some(info) = self.type_decls.shared_types.get(tn) {
                    heap_type = Some(info.heap_type);
                    break;
                }
            }
        }
        // Wildcard fallback (`Some(_) => {}` binds nothing, so no
        // pattern-binding record exists): resolve the payload type from the
        // scrutinee's recorded enum instance type (`Option[Node]`) — same
        // route the inline-result tracker uses.
        if heap_type.is_none() {
            if let Some(te) = self.enum_inst_type_from_span(scrutinee) {
                if let TypeKind::Path(pp) = &te.kind {
                    if pp.segments.last().map(String::as_str) == Some("Option") {
                        if let Some(crate::ast::GenericArg::Type(inner)) =
                            pp.generic_args.as_ref().and_then(|a| a.first())
                        {
                            if let TypeKind::Path(ip) = &inner.kind {
                                if let Some(tn) = ip.segments.last() {
                                    if let Some(info) = self.type_decls.shared_types.get(tn) {
                                        heap_type = Some(info.heap_type);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        let Some(heap_type) = heap_type else {
            return;
        };
        let Some(layout) = self.type_decls.enum_layouts.get("Option") else {
            return;
        };
        let option_ty = layout.llvm_type;
        let some_tag = layout.tags.get("Some").copied().unwrap_or(1);
        let Some(fn_val) = self.current_fn else {
            return;
        };
        let alloca = self.create_entry_alloca(fn_val, "__freshtemp_shared_opt", option_ty.into());
        let _ = self.builder.build_store(alloca, sv);
        if let Some(frame) = self.drop_rc.scope_cleanup_actions.last_mut() {
            frame.push(crate::codegen::state::CleanupAction::RcDecOption {
                name: "__freshtemp_shared_opt".to_string(),
                option_slot: alloca,
                option_ty,
                heap_type,
                some_tag,
            });
        }
    }

    pub(super) fn track_freshtemp_inline_result_scrutinee(
        &mut self,
        scrutinee: &Expr,
        val: BasicValueEnum<'ctx>,
    ) -> Option<PointerValue<'ctx>> {
        if !self.expr_yields_fresh_owned_temp(scrutinee) {
            return None;
        }
        if self.scrutinee_is_borrow_call(scrutinee) {
            return None;
        }
        // B-2026-08-29-6 — a PASSTHROUGH call is a fresh temp only in shape: the
        // callee handed its argument straight back, so the payload still belongs
        // to the named source, which stays armed. Registering the temp's own
        // free here put a second owner on that buffer and both released it
        // (`free(): double free detected in tcache 2`). Excluded for exactly the
        // reason a borrow-returning scrutinee is excluded two lines up — the
        // discarded-temp registrars call these the same aliasing class, and a
        // passthrough is its user-function form.
        //
        // `Result` needs this IN ADDITION to the source-retains classification
        // at the match's scrutinee site: the `Option` spelling of the same
        // program is fixed by that flag alone, because no scrutinee registrar
        // claims an inline `Option` temp, while THIS registrar does claim a
        // `Result` one. Both halves were established by bisecting the shapes
        // individually after a combined fixture still aborted with the first
        // half in place.
        if self.call_passthrough_armed_any_source(scrutinee).is_some() {
            return None;
        }
        let BasicValueEnum::StructValue(sv) = val else {
            return None;
        };
        let te = self.enum_inst_type_from_span(scrutinee)?;
        // Overlay (direct `String`/`Vec` or transparent-wrapper-of-one) payload
        // elems, AND the FULL struct drops for a multi-field / struct-with-heap
        // payload the overlay can't free (B-2026-07-12-2 gap 3: a discarded
        // wide-`T` `AlreadySetError[Rec]` rejected value). Register if EITHER
        // path has heap on EITHER side — the struct-drop half is why we no
        // longer early-return on a `None` overlay.
        let (ok_payload_elem_ty, err_payload_elem_ty) = self
            .result_inline_payload_elems(&te)
            .unwrap_or((None, None));
        let (ok_payload_struct_drop, err_payload_struct_drop) =
            self.result_inline_payload_struct_drops(&te);
        // B-2026-08-25-12 — the INLINE action is only correct for a payload
        // that actually lives inline. A payload wider than `Result`'s 5-word
        // area is heap-BOXED (`coerce_to_payload_words` spills it behind a
        // pointer), and this action's drop runs at `&slot.w0` — the word
        // holding the box POINTER — reading it as the first word of the
        // payload struct. B-2026-08-06-26 established exactly that and gated
        // `track_inline_result_payload_var`, but the gate never reached the
        // other two registration sites, of which this is the FRESH-TEMP
        // scrutinee one: `match f() { Ok(a) => .. }` over a call returning a
        // wide payload. Measured on `Out { v1, v2, v3: Vec[String], tag: i64 }`
        // (10 words): the drop read `{box_ptr, 0, 0}` as Vec #1, zeros as
        // Vec #2, then ran off the END of the 6-word Result alloca for Vec #3
        // and called `free` on whatever followed — a SEGFAULT. Two Vecs stayed
        // inside the alloca and so only mis-freed benign zeros, which is why
        // the fault looked like it began at three fields.
        //
        // Boxed-ness is a property of the payload TYPE, gated per half exactly
        // as the named-binding path does, so a boxed `Ok` beside an
        // inline-heap `Err` keeps the `Err` drop it still needs. The box
        // itself is owned by the `BoxedEnumDrop` registered for this value.
        let (ok_boxed, err_boxed) = Self::result_payload_tes(&te)
            .map(|(ok_te, err_te)| {
                (
                    self.result_payload_is_boxed(&ok_te),
                    self.result_payload_is_boxed(&err_te),
                )
            })
            .unwrap_or((false, false));
        let ok_payload_elem_ty = if ok_boxed { None } else { ok_payload_elem_ty };
        let err_payload_elem_ty = if err_boxed { None } else { err_payload_elem_ty };
        let ok_payload_struct_drop = if ok_boxed {
            None
        } else {
            ok_payload_struct_drop
        };
        let err_payload_struct_drop = if err_boxed {
            None
        } else {
            err_payload_struct_drop
        };
        if ok_payload_elem_ty.is_none()
            && err_payload_elem_ty.is_none()
            && ok_payload_struct_drop.is_none()
            && err_payload_struct_drop.is_none()
        {
            return None;
        }
        let layout = self.type_decls.enum_layouts.get("Result")?;
        let result_ty = layout.llvm_type;
        let ok_tag = layout.tags.get("Ok").copied().unwrap_or(0);
        let err_tag = layout.tags.get("Err").copied().unwrap_or(1);
        // B-2026-08-14-15 leg B, `Result` half — per-element drain for a
        // `Vec[<aggregate>]` payload, per half.
        let (ok_payload_elem_agg_drop, err_payload_elem_agg_drop) =
            match Self::result_payload_tes(&te) {
                Some((ok_te, err_te)) => (
                    if ok_boxed {
                        None
                    } else {
                        self.vec_payload_elem_agg_drop(&ok_te)
                    },
                    if err_boxed {
                        None
                    } else {
                        self.vec_payload_elem_agg_drop(&err_te)
                    },
                ),
                None => (None, None),
            };
        let fn_val = self.current_fn?;
        let alloca = self.create_entry_alloca(fn_val, "__freshtemp_inline_res", result_ty.into());
        let _ = self.builder.build_store(alloca, sv);
        if let Some(frame) = self.drop_rc.scope_cleanup_actions.last_mut() {
            frame.push(
                crate::codegen::state::CleanupAction::FreeInlineResultPayload {
                    result_slot: alloca,
                    result_ty,
                    ok_tag,
                    err_tag,
                    ok_payload_elem_ty,
                    err_payload_elem_ty,
                    ok_payload_struct_drop,
                    err_payload_struct_drop,
                    ok_payload_elem_agg_drop,
                    err_payload_elem_agg_drop,
                },
            );
        }
        Some(alloca)
    }

    /// Does a fresh-temp inline `Result` arm leave its `Ok(_)`/`Err(_)`-bound
    /// payload ONLY BORROWED — read but never moved out (`Err(e) =>
    /// e.rejected.len()`)? Consults the consumption classifier over the arm body
    /// (and guard). Combined with the struct-wrapper check, a borrow-only
    /// struct-wrapper arm skips the source-payload suppression so the source
    /// frees the read-only payload at arm-end (B-2026-07-12-2 gap 2).
    pub(super) fn arm_only_borrows_inline_result_payload(
        &self,
        pattern: &Pattern,
        body: &Expr,
        guard: Option<&Expr>,
    ) -> bool {
        let PatternKind::TupleVariant { patterns, .. } = &pattern.kind else {
            return false;
        };
        let mut binds: Vec<String> = Vec::new();
        for pat in patterns {
            collect_pattern_bindings(pat, &mut binds);
        }
        if binds.is_empty() {
            return false;
        }
        binds.iter().all(|v| {
            super::consume_class::binding_only_borrowed(v, body)
                && guard.is_none_or(|g| super::consume_class::binding_only_borrowed(v, g))
        })
    }

    /// Is the `Ok(_)`/`Err(_)` payload binding of a fresh-temp inline `Result`
    /// arm a user STRUCT (a transparent heap wrapper like `AlreadySetError[
    /// String]`) rather than a DIRECT `String`/`Vec`? A struct-wrapper binding
    /// registers no cleanup of its own, so on a borrow-only read the source must
    /// keep its payload free armed; a direct `String`/`Vec` binding is
    /// `track_vec_var`'d and must always suppress the source (else double-free).
    /// B-2026-07-12-2 gap 2.
    pub(super) fn inline_result_payload_binding_is_struct_wrapper(
        &self,
        pattern: &Pattern,
    ) -> bool {
        let PatternKind::TupleVariant { patterns, .. } = &pattern.kind else {
            return false;
        };
        patterns.iter().any(|p| {
            if matches!(&p.kind, PatternKind::Binding(_)) {
                let key = (p.span.offset, p.span.length);
                if let Some(tn) = self.pattern_state.pattern_binding_types.get(&key) {
                    return self.type_decls.struct_types.contains_key(tn.as_str());
                }
            }
            false
        })
    }

    /// Does the `Ok(_)`/`Err(_)` struct-wrapper payload binding of a fresh-temp
    /// inline `Result` arm register a DROP OF ITS OWN?
    ///
    /// B-2026-08-04-11 leg (b). The wrapper skip in `compile_match` rests on the
    /// premise recorded in
    /// [`Self::inline_result_payload_binding_is_struct_wrapper`]'s doc — "a
    /// struct-wrapper binding registers no cleanup of its own" — so on a
    /// borrow-only read the source must stay armed or the payload leaks. That
    /// premise was true when B-2026-07-12-2 gap 2 wrote it, and B-2026-07-10-3
    /// then falsified it: `bind_pattern_values` now tracks an INLINE
    /// `Option`/`Result` struct payload (`is_inline_optres_struct_payload`)
    /// precisely so its inner `String`/`Vec` fields get freed. Both owners then
    /// free the same buffer — the source's `FreeInlineResultPayload` overlay in
    /// the arm's own block AND the binding's `__karac_drop_<S>` — which aborts
    /// with `free(): double free detected`.
    ///
    /// This mirrors that tracking predicate exactly so the gate can suppress the
    /// source whenever the binding took ownership, and only then: the skip still
    /// applies to a payload the bind site declines to track (not copy-supported,
    /// wider than the inline area and therefore owned by the box drop, or a
    /// `shared enum` view), which is the recover-READ leak the skip exists for.
    pub(super) fn inline_result_payload_binding_registers_own_drop(
        &self,
        pattern: &Pattern,
    ) -> bool {
        let PatternKind::TupleVariant { patterns, .. } = &pattern.kind else {
            return false;
        };
        if self.pattern_state.pattern_binding_scrutinee_is_shared_enum {
            return false;
        }
        patterns.iter().any(|p| {
            if !matches!(&p.kind, PatternKind::Binding(_)) {
                return false;
            }
            let key = (p.span.offset, p.span.length);
            let Some(tn) = self.pattern_state.pattern_binding_types.get(&key) else {
                return false;
            };
            let tn = tn.as_str();
            if !self.type_decls.struct_types.contains_key(tn)
                || self.type_decls.shared_types.contains_key(tn)
            {
                return false;
            }
            // The by-name pair the bind site tracks unconditionally.
            if matches!(tn, "Response" | "HttpError") {
                return true;
            }
            self.aggregate_param_copy_supported_struct(tn, &mut Vec::new())
                && self.type_decls.struct_types.get(tn).is_some_and(|st| {
                    Self::llvm_type_word_count((*st).into())
                        <= self.pattern_state.pattern_binding_scrutinee_optres_area
                })
        })
    }

    /// Does a fresh-temp inline `Result` arm MOVE a HEAP field of its
    /// `Ok(_)`/`Err(_)` struct-wrapper binding into a FREE-FUNCTION call
    /// argument (`println(w.s)` / `g(w.s)`)? `arm_only_borrows_inline_result_
    /// payload` delegates to `consume_class`, which scores a free-fn arg as
    /// entry-copied / non-consuming — so a `println(w.s)` arm reads as
    /// borrow-only and the wrapper gate KEEPS the source payload drop armed.
    /// But a heap field passed BY VALUE to a free fn is MOVED, and the callee
    /// frees its buffer, so the still-armed `FreeInlineResultPayload` frees it
    /// a second time (B-2026-07-23-4). This detects exactly that shape so the
    /// gate suppresses the source drop, leaving the callee sole owner. The
    /// aggregate / return / let-move / method-arg move-out shapes already score
    /// as consuming (`borrow_only == false`), so this covers only the free-fn
    /// arg gap. Inline `Result`/`Option` struct payloads hold at most one
    /// `String`/`Vec` (two would be 6 words, over the 5-word inline area), so
    /// the whole-payload-area suppression this drives frees nothing it should
    /// not.
    pub(super) fn wrapper_arm_moves_heap_field_to_free_fn(
        &self,
        pattern: &Pattern,
        body: &Expr,
        guard: Option<&Expr>,
    ) -> bool {
        let PatternKind::TupleVariant { patterns, .. } = &pattern.kind else {
            return false;
        };
        // Struct-wrapper payload bindings, paired with their struct type name.
        let mut names: Vec<String> = Vec::new();
        let mut binding_types: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for p in patterns {
            if let PatternKind::Binding(name) = &p.kind {
                let key = (p.span.offset, p.span.length);
                if let Some(tn) = self.pattern_state.pattern_binding_types.get(&key) {
                    if self.type_decls.struct_types.contains_key(tn.as_str()) {
                        names.push(name.clone());
                        binding_types.insert(name.clone(), tn.clone());
                    }
                }
            }
        }
        if names.is_empty() {
            return false;
        }
        let mut pairs = super::consume_class::binding_fields_passed_to_free_fn_arg(&names, body);
        if let Some(g) = guard {
            pairs.extend(super::consume_class::binding_fields_passed_to_free_fn_arg(
                &names, g,
            ));
        }
        pairs.iter().any(|(root, field)| {
            binding_types
                .get(root)
                .is_some_and(|tn| self.struct_field_is_heap(tn, field))
        })
    }

    /// Does a fresh-temp inline `Result` arm pass its WHOLE `Ok(_)`/`Err(_)`
    /// struct-wrapper binding BY VALUE to a free function (`Ok(s) => take(s)`)?
    ///
    /// The whole-binding sibling of `wrapper_arm_moves_heap_field_to_free_fn`,
    /// and it exists for the same reason one level up: `consume_class` scores a
    /// free-fn argument as entry-copied, so the arm reads as borrow-only and the
    /// wrapper gate keeps the source payload drop armed — but a by-value struct
    /// param is CALLEE-OWNED, and the callee ends with
    /// `__karac_drop_struct_<S>`, which frees the field's heap. Both then free
    /// it: measured as a use-after-free in `karac_map_free_with_drop_vec`,
    /// surfacing as a plain SEGV on a default build (B-2026-08-11-29).
    ///
    /// Why the field sibling did not already cover it: it looks for
    /// `take(s.a)`, and this shape passes `s` itself.
    ///
    /// Why a `Vec`-field wrapper never crashed, and why this does not change it:
    /// a Vec/String field makes the struct entry-copy supported, so
    /// `inline_result_payload_binding_registers_own_drop` is already true and
    /// suppression already fires. `Map`/`Set` fields are excluded from that
    /// predicate (`field_copy_supported`), which is what left this shape as the
    /// only one reaching the arm with the source drop still armed. Adding a
    /// second true term to a condition that is already true for Vec is a no-op
    /// there.
    fn wrapper_arm_moves_whole_binding_to_free_fn(
        &self,
        pattern: &Pattern,
        body: &Expr,
        guard: Option<&Expr>,
    ) -> bool {
        let PatternKind::TupleVariant { patterns, .. } = &pattern.kind else {
            return false;
        };
        let mut names: Vec<String> = Vec::new();
        for p in patterns {
            let PatternKind::Binding(name) = &p.kind else {
                continue;
            };
            let key = (p.span.offset, p.span.length);
            let Some(tn) = self.pattern_state.pattern_binding_types.get(&key) else {
                continue;
            };
            // A non-shared user struct only: a shared handle is refcounted (the
            // callee does not free it), and a non-struct payload never reaches
            // the wrapper gate at all.
            if self.type_decls.struct_types.contains_key(tn.as_str())
                && !self.type_decls.shared_types.contains_key(tn.as_str())
            {
                names.push(name.clone());
            }
        }
        if names.is_empty() {
            return false;
        }
        let mut moved = super::consume_class::bindings_passed_whole_to_free_fn_arg(&names, body);
        if let Some(g) = guard {
            moved.extend(super::consume_class::bindings_passed_whole_to_free_fn_arg(
                &names, g,
            ));
        }
        !moved.is_empty()
    }

    /// Is field `field` of struct `struct_name` a heap-owning `String`/`Vec`/
    /// `VecDeque` (i.e. a `{ptr,len,cap}` buffer whose drop frees the pointer)?
    /// Resolved by field type NAME first, then by concrete LLVM shape (through
    /// the active monomorph subst) so a generic field whose spelled name is
    /// `str` — or any type that lowers to the vec struct — is still recognised.
    pub(super) fn struct_field_is_heap(&self, struct_name: &str, field: &str) -> bool {
        let Some(field_names) = self.type_decls.struct_field_names.get(struct_name) else {
            return false;
        };
        let Some(idx) = field_names.iter().position(|n| n == field) else {
            return false;
        };
        if let Some(tn) = self
            .type_decls
            .struct_field_type_names
            .get(struct_name)
            .and_then(|v| v.get(idx))
            .and_then(|o| o.as_deref())
        {
            if matches!(tn, "String" | "str" | "Vec" | "VecDeque") {
                return true;
            }
        }
        if let Some(te) = self
            .type_decls
            .struct_field_type_exprs
            .get(struct_name)
            .and_then(|v| v.get(idx))
        {
            let te = self.subst_monomorph_type_params(te);
            return self.llvm_type_for_type_expr(&te) == self.vec_struct_type().into();
        }
        false
    }

    /// Alloca-keyed sibling of `suppress_inline_result_payload_cleanup` for a
    /// FRESH-TEMP inline `Result` scrutinee (no identifier to resolve). Zeros
    /// the materialized scrutinee slot's `cap` (field 3) on a CONSUMING `Ok`/
    /// `Err` arm so its `FreeInlineResultPayload` skips — the binding / consumer
    /// now owns the payload buffer (B-2026-07-12-2 gap 2a). A no-bind / `_` arm
    /// is NOT consuming, so the free stays armed and reclaims the discarded
    /// payload. The CALLER gates a borrow-only STRUCT-WRAPPER arm out.
    pub(super) fn suppress_inline_result_payload_cleanup_at(
        &self,
        slot: PointerValue<'ctx>,
        pattern: &Pattern,
    ) {
        let PatternKind::TupleVariant { path, patterns } = &pattern.kind else {
            return;
        };
        let variant = path.last().map(|s| s.as_str());
        if variant != Some("Ok") && variant != Some("Err") {
            return;
        }
        if !patterns.iter().any(pattern_consumes_field) {
            return;
        }
        let Some(layout) = self.type_decls.enum_layouts.get("Result") else {
            return;
        };
        self.zero_result_payload_area(layout.llvm_type, slot, "respl.suppress.at");
    }

    /// Zero every payload word of a materialized `Result` scrutinee slot
    /// (fields 1..=payload-area). The overlay `FreeInlineResultPayload` only
    /// needs its `cap` word (field 3) at zero to skip, but a struct-drop payload
    /// (B-2026-07-12-2 gap 3) caps-guards on the CONCRETE struct's heap-field
    /// offsets — which vary — so zeroing the whole area disarms BOTH shapes on a
    /// consuming move-out arm without knowing the field layout. Safe superset:
    /// the overlay's `cap` word is inside the zeroed range.
    pub(super) fn zero_result_payload_area(
        &self,
        result_ty: inkwell::types::StructType<'ctx>,
        slot: PointerValue<'ctx>,
        name: &str,
    ) {
        let i64_t = self.context.i64_type();
        let zero = i64_t.const_int(0, false);
        let n_fields = result_ty.count_fields();
        for f in 1..n_fields {
            if let Ok(word_ptr) =
                self.builder
                    .build_struct_gep(result_ty, slot, f, &format!("{name}.w{f}"))
            {
                let _ = self.builder.build_store(word_ptr, zero);
            }
        }
    }

    /// Wholesale-drop a fresh-temp enum scrutinee on a *miss* edge — the
    /// pattern did not match, so nothing was bound out and the entire
    /// value's heap is freed by a single `__karac_drop_<E>` call (no
    /// cap-suppression, unlike the match edge). Used by
    /// `compile_while_let`'s loop-exit block: the final non-matching
    /// scrutinee is evaluated in the header and never enters the
    /// per-iteration body frame, so without this its heap leaks (B
    /// follow-up #2 — the `while let` heap-bearing miss variant). Unlike
    /// `materialize_freshtemp_enum_scrutinee`, this emits the drop call
    /// inline rather than registering a `track_enum_var` cleanup action,
    /// because the miss edge is a one-shot exit, not a scope whose frame
    /// drains. Same fresh-temp / non-shared / has-heap gate, so it is a
    /// no-op for place scrutinees (owned elsewhere — a wholesale free
    /// would double-free against that owner's cleanup) and for heap-free
    /// enums. The builder must be positioned at the miss block.
    ///
    /// `force` (B-2026-07-21-8): the while-let ref-chain clone leg evaluates
    /// its deep clone in the HEADER, so the final non-matching evaluation's
    /// copy is an owned temp this drop must free even though the scrutinee
    /// EXPR is a place (the fresh-temp gate would wrongly bail).
    /// The concrete `Option[..]` / `Result[.., ..]` TypeExpr the type-checker
    /// recorded for `e`, if any — B-2026-09-02-12's bridge from a scrutinee
    /// expression to the payload type its envelope erases.
    ///
    /// The same map `track_discarded_optres_payload_bodies` reads for the
    /// discard spelling, keyed the same way, so a declined match and a bare
    /// discard of the identical call resolve to the identical payload walker.
    pub(super) fn optres_inst_type_expr(&self, e: &Expr) -> Option<crate::ast::TypeExpr> {
        self.type_decls
            .enum_inst_type_exprs
            .get(&(e.span.offset, e.span.length))
            .cloned()
    }

    /// B-2026-09-02-12 — run the PAYLOAD BODIES of a fresh-temp
    /// `Option`/`Result` scrutinee that the pattern declined, given the slot it
    /// was materialized into.
    ///
    /// The bodies-only sibling of
    /// [`Self::drop_freshtemp_enum_scrutinee_on_miss`], for the two legs whose
    /// MEMORY half is already handled elsewhere (`if let`'s
    /// `emit_taken_scrutinee_drop` at the top of the else arm, and
    /// `let ... else`'s at the top of the else block). Calling the full helper
    /// there would emit a second drop switch over the same slot; calling
    /// nothing left the body unrun, which is the defect.
    ///
    /// MISS EDGE ONLY. On the hit edge the arm's binding owns the payload and
    /// runs the body itself, so this must not be emitted there.
    ///
    /// Ordered BEFORE the memory drop at both call sites, the B-2026-08-01-2
    /// rule: a body that reads a heap payload must run before the switch that
    /// frees it.
    pub(super) fn run_optres_freshtemp_payload_bodies_on_miss(
        &mut self,
        scrutinee: &Expr,
        val: BasicValueEnum<'ctx>,
    ) {
        if !self.expr_yields_fresh_owned_temp(scrutinee) {
            return;
        }
        // A borrow accessor's `Option` payload ALIASES a container element the
        // container still owns and still walks; firing here would double it.
        if self.scrutinee_is_borrow_call(scrutinee) {
            return;
        }
        let BasicValueEnum::StructValue(sv) = val else {
            return;
        };
        let Some(te) = self.optres_inst_type_expr(scrutinee) else {
            return;
        };
        let Some(bodies) = self.emit_optres_payload_user_drop_bodies_fn(&te) else {
            return;
        };
        let Some(fn_val) = self.current_fn else {
            return;
        };
        // Its own slot rather than a caller-supplied one: an ALL-SCALAR
        // `Result[W, W]` materializes nowhere on this path — every existing
        // fresh-temp tracker here is a HEAP tracker and declines it — so there
        // is no slot to be handed. The walker takes a pointer, so the value is
        // spilled here. Nothing frees this copy: it is bodies-only, and the
        // memory half runs over the caller's own value.
        let slot = self.create_entry_alloca(fn_val, "__optres_miss_bodies", sv.get_type().into());
        let _ = self.builder.build_store(slot, sv);
        self.builder.build_call(bodies, &[slot.into()], "").unwrap();
    }

    pub(super) fn drop_freshtemp_enum_scrutinee_on_miss(
        &mut self,
        scrutinee: &Expr,
        pattern: &Pattern,
        val: BasicValueEnum<'ctx>,
        force: bool,
    ) {
        if !force && !self.expr_yields_fresh_owned_temp(scrutinee) {
            return;
        }
        let BasicValueEnum::StructValue(sv) = val else {
            return;
        };
        let Some(enum_name) = self.variant_pattern_enum_name(pattern) else {
            return;
        };
        // Snapshot the layout bits before the mutable `emit_enum_drop_switch`
        // borrow; `is_shared` enums use the RC path, not the drop switch.
        let (llvm_ty, is_shared) = match self.type_decls.enum_layouts.get(&enum_name) {
            Some(l) => (l.llvm_type, l.is_shared),
            None => return,
        };
        if is_shared {
            return;
        }
        // B-2026-09-01-42 — THREE COMPLEMENTARY ACTIONS, NOT ONE. This emitted
        // only the payload-walking drop SWITCH, which is the memory half; the
        // enum's own user `Drop` body and its payload's bodies walk were never
        // run, so the loop-exit temporary was freed silently. design.md
        // § `if let` and `let...else` > "Scrutinee temporary scope", fourth
        // bullet, requires the final non-matching temporary to be dropped, and
        // "dropped" includes its body.
        //
        // Exactly the split B-2026-06-10's enum arm fixed for a call-argument
        // enum temporary (`call_dispatch.rs`, the `fresh_enum_temp` branch),
        // whose comment states the rule this reuses: UserDrop and EnumDrop are
        // COMPLEMENTARY for an enum, not mutually exclusive, because the
        // wrapper's field-cleanup half (`emit_struct_drop_synthesis`) is a
        // no-op for an enum type name.
        //
        // THE HEAP-FREE ENUM IS WHY THE EARLY RETURN MOVED rather than gaining
        // a companion. `emit_enum_drop_switch` answers `None` for an enum with
        // no heap-bearing variant, and this returned on it — so the row's own
        // repro (`enum E { A(R), B }`, `R { id: i64 }`, all scalar) registered
        // NOTHING at all. Bailing now requires all three to be absent.
        let drop_fn = self.emit_enum_drop_switch(&enum_name);
        let user_wrapper = self
            .drop_rc
            .user_drop_wrapper_fns
            .get(enum_name.as_str())
            .copied();
        // B-2026-09-02-12 — `Option`/`Result` reach their payload bodies
        // through the INSTANTIATION, not through the enum's declaration.
        //
        // This used to be a flat `None`, on the reading that the two keep
        // "their own payload machinery, the same carve-out the call-argument
        // precedent makes". The carve-out is right about the WALKER and wrong
        // about the outcome: `emit_enum_payload_user_drop_bodies_fn` is
        // declared-type driven and skips `Ok(T)` / `Err(E)` because those
        // payloads are the enum's own generic parameters, so routing them
        // there would have been a no-op — but routing them NOWHERE meant a
        // declined `Result` temporary ran no body at all, on any surface,
        // where the `mkerr();` discard spelling of the same value runs one.
        //
        // `emit_optres_payload_user_drop_bodies_fn` takes the concrete
        // `Result[W, W]` the type-checker recorded for this expression, which
        // is the same resolution `track_discarded_optres_payload_bodies` makes
        // for the discard spelling — so the two spellings agree by
        // construction. An expression with no recorded instantiation (an
        // erased generic) still answers `None`, which keeps both backends
        // silent there, the safe direction.
        let payload_bodies = if enum_name != "Option" && enum_name != "Result" {
            self.emit_enum_payload_user_drop_bodies_fn(&enum_name)
        } else {
            self.optres_inst_type_expr(scrutinee)
                .and_then(|te| self.emit_optres_payload_user_drop_bodies_fn(&te))
        };
        if drop_fn.is_none() && user_wrapper.is_none() && payload_bodies.is_none() {
            return;
        }
        let Some(fn_val) = self.current_fn else {
            return;
        };
        let alloca = self.create_entry_alloca(fn_val, "__whilelet_miss_scrut", llvm_ty.into());
        let _ = self.builder.build_store(alloca, sv);
        // ORDER IS MEASURED, not assumed: a bound local of the very value that
        // ends the loop prints `dB dW7` on both backends — the enum's own body,
        // then the payload's. That is what the frame drain produces for the
        // let-path (LIFO over memory, walker, own-body), and the miss edge
        // emits inline rather than through a frame, so the sequence is written
        // out here directly. Bodies precede the switch that frees what they
        // read (the B-2026-08-01-2 rule).
        if let Some(w) = user_wrapper {
            self.builder.build_call(w, &[alloca.into()], "").unwrap();
        }
        if let Some(w) = payload_bodies {
            self.builder.build_call(w, &[alloca.into()], "").unwrap();
        }
        if let Some(d) = drop_fn {
            self.builder.build_call(d, &[alloca.into()], "").unwrap();
        }
    }
}

/// Whether a payload-position sub-pattern *consumes* ownership of its
/// field — used by `suppress_destructured_enum_payload_cleanup` to
/// decide whether to neutralize the source enum's drop for that
/// field. Consumption flow:
///
/// - `Binding` / `AtBinding` — yes, the name now owns the value.
/// - `Tuple` / `TupleVariant` / `Struct` — yes if any inner pattern
///   consumes; the destructure binds parts of the composite, the
///   composite's cleanup (recorded by `track_vec_var` / similar on
///   the new bindings) frees the heap content.
/// - `Or` — yes (conservative); each alternative is its own arm with
///   its own consumption pattern.
/// - `Wildcard`, `Literal`, `RangePattern`, `Slice` — no; the field
///   wasn't claimed by the destructure, so the source's drop must
///   still free its heap content.
fn pattern_consumes_field(p: &crate::ast::Pattern) -> bool {
    match &p.kind {
        PatternKind::Wildcard
        | PatternKind::Literal(_)
        | PatternKind::RangePattern { .. }
        | PatternKind::Slice { .. } => false,
        PatternKind::Binding(_) => true,
        // `ref name @ PATTERN` — the whole subtree borrows (design.md
        // § @ Bindings); nothing is moved out, so the source's drop
        // must still free the field's heap content.
        PatternKind::AtBinding { by_ref: true, .. } => false,
        PatternKind::AtBinding { pattern, .. } => pattern_consumes_field(pattern),
        PatternKind::Tuple(pats) => pats.iter().any(pattern_consumes_field),
        PatternKind::TupleVariant { patterns, .. } => patterns.iter().any(pattern_consumes_field),
        PatternKind::Struct { fields, .. } => fields.iter().any(|f| {
            f.pattern
                .as_ref()
                .map(pattern_consumes_field)
                .unwrap_or(true) // shorthand `Field` means a binding by field name
        }),
        PatternKind::Or(pats) => pats.iter().any(pattern_consumes_field),
    }
}

/// Collect the OWNING binding names a pattern introduces (the leaves that
/// `pattern_consumes_field` counts). `ref name @ …` and its subtree borrow, so
/// they introduce no owning binding and are skipped. Used by the caller-retains
/// consumption gate to know which variables to check in an arm body.
pub(super) fn collect_pattern_bindings(p: &crate::ast::Pattern, out: &mut Vec<String>) {
    match &p.kind {
        PatternKind::Binding(n) => out.push(n.clone()),
        PatternKind::AtBinding { by_ref: true, .. } => {}
        PatternKind::AtBinding {
            by_ref: false,
            name,
            pattern,
        } => {
            out.push(name.clone());
            collect_pattern_bindings(pattern, out);
        }
        PatternKind::Tuple(pats) | PatternKind::TupleVariant { patterns: pats, .. } => {
            for sp in pats {
                collect_pattern_bindings(sp, out);
            }
        }
        PatternKind::Struct { fields, .. } => {
            for f in fields {
                match &f.pattern {
                    Some(sp) => collect_pattern_bindings(sp, out),
                    None => out.push(f.name.clone()), // shorthand binds by field name
                }
            }
        }
        PatternKind::Or(pats) => {
            for sp in pats {
                collect_pattern_bindings(sp, out);
            }
        }
        PatternKind::Wildcard
        | PatternKind::Literal(_)
        | PatternKind::RangePattern { .. }
        | PatternKind::Slice { .. } => {}
    }
}

/// The surface names that carry no owner: every integer width, every float
/// width, `bool` and `char`. Shared by the payload-binding entry point and the
/// struct/enum field walker so the two cannot drift apart — the drift between
/// them was B-2026-08-30-47.
fn is_scalar_surface_name(name: &str) -> bool {
    matches!(
        name,
        "i8" | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "isize"
            | "f16"
            | "bf16"
            | "f32"
            | "f64"
            | "bool"
            | "char"
    )
}

/// Which shape a BOXED `Option`/`Result` payload has, for
/// [`Codegen::suppress_boxed_payload_struct_destructure_at`]. The two arms
/// disarm the same storage — the box interior — and differ only in how a
/// consumed field is located: by struct field name, or by the live variant's
/// word offsets (B-2026-08-31-23).
enum BoxedPayloadShape {
    Struct(String),
    EnumVariant(String),
}
