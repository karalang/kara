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

use inkwell::basic_block::BasicBlock;
use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
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
        let tail = self.tail_ret_inner.take();
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
                if self.ref_params.contains_key(name) {
                    let pointee = *self.ref_params.get(name).unwrap();
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
        let saved_scrut_enum_hint = self.match_scrutinee_enum_hint.take();
        self.match_scrutinee_enum_hint = self
            .type_name_of_expr(scrutinee)
            .filter(|n| self.enum_layouts.contains_key(n.as_str()));
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
        let did_clone_borrowed_index_field =
            did_clone_borrowed_index_field || did_clone_loop_elem || did_clone_ref_chain;
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
        // Oversized-enum-payload §1/§2: a fresh-temp scrutinee whose payload was
        // heap-boxed (Option[Wide] / Result[Wide,_]) needs the box freed too.
        // Mutually exclusive with the user-enum path above — seeded Option /
        // Result have all-`None` drop kinds, so `materialize_freshtemp_enum_
        // scrutinee` returns None for them; the gate makes that explicit.
        if scrut_ref_ptr.is_none() && freshtemp_enum.is_none() {
            let pats: Vec<&Pattern> = arms.iter().map(|a| &a.pattern).collect();
            self.track_freshtemp_boxed_enum_scrutinee(scrutinee, &pats, scrut);
        }
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
        if scrut_ref_ptr.is_none() && freshtemp_enum.is_none() && freshtemp_inline_res.is_none() {
            let pats: Vec<&Pattern> = arms.iter().map(|a| &a.pattern).collect();
            self.track_freshtemp_shared_option_scrutinee(scrutinee, &pats, scrut);
        }
        // Detect borrow-returning scrutinees so pattern bindings don't
        // register a `FreeVecBuffer` against a buffer the container still
        // owns. `Map.get` is the canonical case (the returned `Option[V]`
        // aliases the bucket entry's value words); a duplicate cleanup
        // would double-free against the `karac_map_free_with_val_drop_vec`
        // path at function exit.
        let saved_borrow_flag = self.pattern_binding_is_borrow;
        self.pattern_binding_is_borrow = self.scrutinee_is_borrow_call(scrutinee)
            || self.scrutinee_is_borrowed_binding(scrutinee)
            || self.scrutinee_is_readonly_borrowed_place(scrutinee, arms)
            || self.scrutinee_is_readonly_owned_agg_loop_var(scrutinee, arms);
        // B-2026-07-15-21 Part B — scrutinee is an RC-elidable borrowed param:
        // skip the Some-binding acquire + its scope-exit RcDec (payload is a
        // proven-non-escaping alias of the caller-kept-alive param), which also
        // clears the post-call release epilogue so tailcallelim can loop-ify the
        // tail recursion.
        let saved_elidable_param_flag = self.pattern_binding_scrutinee_is_elidable_param;
        self.pattern_binding_scrutinee_is_elidable_param =
            self.scrutinee_is_elidable_param(scrutinee);
        // B-2026-06-13-13 residual A: when the scrutinee is the type-erased
        // `Option`/`Result`, its payload is owned by the dedicated inline/boxed
        // cleanup, not a per-field `EnumDrop` — so the pattern-binding struct
        // track must skip a bound struct payload to avoid double-freeing. Same
        // enum for every arm, so resolved once from any variant arm.
        let saved_opt_res_flag = self.pattern_binding_scrutinee_is_option_result;
        self.pattern_binding_scrutinee_is_option_result = arms.iter().any(|a| {
            matches!(
                self.variant_pattern_enum_name(&a.pattern).as_deref(),
                Some("Option") | Some("Result")
            )
        });
        // B-2026-07-10-3 — record the seed enum's inline payload-area budget
        // (Option = 3, Result = 5) so `bind_pattern_values` can tell an INLINE
        // struct payload (safe to `track_struct_var`) from a heap-BOXED one
        // (owned by the box drop). Same enum for every arm, resolved once.
        let saved_optres_area = self.pattern_binding_scrutinee_optres_area;
        self.pattern_binding_scrutinee_optres_area = arms
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
        let saved_shared_enum_flag = self.pattern_binding_scrutinee_is_shared_enum;
        self.pattern_binding_scrutinee_is_shared_enum = arms.iter().any(|a| {
            self.variant_pattern_enum_name(&a.pattern)
                .and_then(|n| self.shared_types.get(&n).cloned())
                .is_some_and(|i| i.is_enum)
        });
        let fn_val = self.current_fn.unwrap();
        let merge_bb = self.context.append_basic_block(fn_val, "match.merge");

        let mut arm_results: Vec<(BasicValueEnum<'ctx>, BasicBlock<'ctx>)> = Vec::new();

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

        for (i, arm) in arms.iter().enumerate() {
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
            self.scope_cleanup_actions.push(Vec::new());

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
                let handled_via_ptr = if let Some((scrut_ptr, pointee_ty)) = scrut_ref_ptr {
                    self.bind_pattern_values_via_ptr(&arm.pattern, scrut_ptr, pointee_ty)?
                        .is_some()
                } else {
                    false
                };
                if !handled_via_ptr {
                    self.bind_pattern_values(&arm.pattern, scrut)?;
                    // Slice 3s (B-2026-07-01-12): a borrow-mode `Some(x)` bind
                    // over a `Map.get` scrutinee whose arm (guard or body)
                    // MOVES `x` gets a deep clone + owned tracking — the
                    // escaping value must not alias the bucket's storage.
                    if self.pattern_binding_is_borrow {
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
                if self.pattern_binding_is_borrow {
                    self.register_borrowed_agg_payload_struct_bindings(&arm.pattern);
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
            if scrut_ref_ptr.is_none() && !self.pattern_binding_is_borrow {
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
                } else {
                    self.suppress_destructured_enum_payload_cleanup(scrutinee, &arm.pattern);
                    // B-2026-06-10-6 companion: the erased-`Option` drop
                    // switch can't classify an inline `String`/`Vec` payload,
                    // so the suppression above no-ops for it. Zero the source
                    // `Option`'s `cap` here when an arm binds the `Some`
                    // payload out — its `FreeInlineOptionPayload` scope-exit
                    // free would otherwise double-free against the binding.
                    self.suppress_inline_option_payload_cleanup(scrutinee, &arm.pattern);
                    self.suppress_inline_result_payload_cleanup(scrutinee, &arm.pattern);
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
                        if !(borrow_only && wrapper) {
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
                    // Slice 3t: struct-destructure of a BOXED payload — zero
                    // the consumed fields inside the box so the binding's
                    // BoxedEnumDrop inner walk frees only unbound fields.
                    self.suppress_boxed_payload_struct_destructure(scrutinee, &arm.pattern);
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
                let optres_bindings_owned = !self.pattern_binding_is_borrow;
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

            let mut arm_val = self.compile_tail_final_expr(&arm.body, tail)?;
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
                self.suppress_source_vec_cleanup_for_arg(&arm.body);
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
                arm_val = self.deepcopy_owned_param_branch_tail(&arm.body, arm_val);
                // Re-read the current bb AFTER drain — the cleanup IR
                // may have appended new basic blocks (e.g. `cleanup.free`
                // / `cleanup.skip` for FreeVecBuffer's `cap > 0` guard),
                // so the merge-predecessor is the drain's exit bb, NOT
                // `arm_body_end`. The PHI at `merge_bb` must list the
                // ACTUAL predecessor bb where the unconditional branch
                // to merge originates from, or LLVM module verification
                // fails with "PHI node entries do not match predecessors".
                let merge_pred = self.builder.get_insert_block().unwrap();
                arm_results.push((arm_val, merge_pred));
                self.builder.build_unconditional_branch(merge_bb).unwrap();
            } else {
                // Early-return / terminator inside arm body: the return
                // path's own `emit_scope_cleanup` walked the entire stack
                // including this per-arm frame and emitted cleanup for
                // its actions before the return. Pop the now-spent frame
                // so it doesn't shadow subsequent arms' bindings.
                self.scope_cleanup_actions.pop();
            }
            // B-2026-07-13-6: revert this arm's pattern/body binds (see the
            // per-arm snapshot above) so the next arm and the post-`match` code
            // resolve outer names, not this arm's shadows.
            self.restore_var_env(arm_snap);
        }

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

        self.builder.position_at_end(merge_bb);
        self.pattern_binding_is_borrow = saved_borrow_flag;
        self.pattern_binding_scrutinee_is_elidable_param = saved_elidable_param_flag;
        self.pattern_binding_scrutinee_is_option_result = saved_opt_res_flag;
        self.pattern_binding_scrutinee_optres_area = saved_optres_area;
        self.pattern_binding_scrutinee_is_shared_enum = saved_shared_enum_flag;
        self.match_scrutinee_enum_hint = saved_scrut_enum_hint;

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
            return Ok(phi.as_basic_value());
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
                    self.memcmp_fn,
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
        self.ref_params.contains_key(name)
            || self.for_loop_borrow_vars.contains(name)
            || self.borrow_accessor_let_payload.contains_key(name)
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
        self.rc_elide_ref_params
            .get(&self.current_fn_name)
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
                self.borrowed_agg_payload_struct_vars.insert(n);
            }
        }
    }

    /// Collect `Binding` leaf names that sit in a variant/struct PAYLOAD
    /// position (inside a `TupleVariant` / `Struct` pattern), skipping a
    /// top-level whole-scrutinee binding (`other => …`). `in_payload` becomes
    /// true once a variant/struct sub-pattern is entered.
    fn collect_variant_payload_binding_names(
        pattern: &Pattern,
        in_payload: bool,
        out: &mut Vec<String>,
    ) {
        match &pattern.kind {
            PatternKind::Binding(n) => {
                if in_payload {
                    out.push(n.clone());
                }
            }
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
        if !self.for_loop_owned_agg_vars.contains(name.as_str()) {
            return false;
        }
        self.no_arm_payload_escapes(arms)
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
        if !self.for_loop_owned_agg_vars.contains(name.as_str()) {
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
        let Some(layout) = self.enum_layouts.get(&enum_name).cloned() else {
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
        if !self.signature_ref_params.contains(root) {
            return (val, false);
        }
        if !escapes {
            return (val, false);
        }
        let Some(enum_name) = self.place_chain_type_name(scrutinee) else {
            return (val, false);
        };
        let Some(layout) = self.enum_layouts.get(&enum_name).cloned() else {
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
        if !self.signature_ref_params.contains(root) {
            return (val, None);
        }
        if !escapes {
            return (val, None);
        }
        let Some(struct_name) = self.place_chain_type_name(scrutinee) else {
            return (val, None);
        };
        if !self.struct_types.contains_key(struct_name.as_str())
            || self.shared_types.contains_key(struct_name.as_str())
            || self.enum_layouts.contains_key(struct_name.as_str())
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
                span: scrutinee.span.clone(),
            }),
            span: scrutinee.span.clone(),
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
        if !self.signature_ref_params.contains(root) {
            return (val, None);
        }
        if !escapes {
            return (val, None);
        }
        let Some(obj_ty) = self.place_chain_type_name(object) else {
            return (val, None);
        };
        let Some(idx) = self
            .struct_field_names
            .get(obj_ty.as_str())
            .and_then(|ns| ns.iter().position(|n| n == field))
        else {
            return (val, None);
        };
        let Some(field_te) = self
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
        let Some(layout) = self.enum_layouts.get("Option") else {
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
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(
                crate::codegen::state::CleanupAction::FreeInlineOptionPayload {
                    option_slot: slot,
                    option_ty,
                    some_tag,
                    payload_elem_ty: Some(payload_elem_ty),
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
        if !self.signature_ref_params.contains(root) {
            return (val, None);
        }
        if !escapes {
            return (val, None);
        }
        let Some(obj_ty) = self.place_chain_type_name(object) else {
            return (val, None);
        };
        let Some(idx) = self
            .struct_field_names
            .get(obj_ty.as_str())
            .and_then(|ns| ns.iter().position(|n| n == field))
        else {
            return (val, None);
        };
        let Some(field_te) = self
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
        if !self.signature_ref_params.contains(root) {
            return val;
        }
        let Some(leaf) = self.place_chain_type_name(value) else {
            return val;
        };
        if self.shared_types.contains_key(leaf.as_str()) {
            return val;
        }
        // Enum leaf: in-place payload deep-copy (same duplication the
        // scrutinee clone uses — copy-depth == drop-depth). Seeded
        // Option/Result are NOT this leg (their erased layouts carry
        // all-None drop kinds) — Option falls through to the field-te leg
        // below, Result stays status quo (no deep clone in the dispatcher).
        if !matches!(leaf.as_str(), "Option" | "Result") {
            if let Some(layout) = self.enum_layouts.get(leaf.as_str()).cloned() {
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
            let ExprKind::FieldAccess { object, field } = &value.kind else {
                return val;
            };
            let Some(obj_ty) = self.place_chain_type_name(object) else {
                return val;
            };
            let Some(fte) = self
                .struct_field_names
                .get(obj_ty.as_str())
                .and_then(|ns| ns.iter().position(|n| n == field))
                .and_then(|idx| {
                    self.struct_field_type_exprs
                        .get(obj_ty.as_str())
                        .and_then(|tes| tes.get(idx))
                })
                .cloned()
            else {
                return val;
            };
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
        } else if self.struct_types.contains_key(leaf.as_str()) {
            // Struct leaf: dispatcher deep clone keyed on the bare name.
            TypeExpr {
                kind: TypeKind::Path(crate::ast::PathExpr {
                    segments: vec![leaf.clone()],
                    generic_args: None,
                    span: value.span.clone(),
                }),
                span: value.span.clone(),
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
        if !self.signature_ref_params.contains(root) {
            return (val, None);
        }
        if !escapes {
            return (val, None);
        }
        let Some(obj_ty) = self.place_chain_type_name(object) else {
            return (val, None);
        };
        let Some(fte) = self
            .struct_field_names
            .get(obj_ty.as_str())
            .and_then(|ns| ns.iter().position(|n| n == field))
            .and_then(|idx| {
                self.struct_field_type_exprs
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
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
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
                        if ip.segments.last().is_some_and(|n| self.shared_types.contains_key(n.as_str())))
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
        let ExprKind::FieldAccess { object, field } = &value.kind else {
            return None;
        };
        let obj_ty = self.place_chain_type_name(object)?;
        let idx = self
            .struct_field_names
            .get(obj_ty.as_str())?
            .iter()
            .position(|n| n == field)?;
        let field_te = self
            .struct_field_type_exprs
            .get(obj_ty.as_str())?
            .get(idx)?
            .clone();
        let TypeKind::Path(p) = &field_te.kind else {
            return None;
        };
        let is_result = match p.segments.last().map(|s| s.as_str()) {
            Some("Option") => {
                if self.option_inline_payload_elem(&field_te).is_none()
                    || self
                        .option_inner_shared_type_for_type_expr(&field_te)
                        .is_some()
                {
                    return None;
                }
                false
            }
            Some("Result") => {
                if !self.result_field_direct_vecstr_halves_ok(&field_te) {
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
            let Some(layout) = self.enum_layouts.get("Result") else {
                return;
            };
            let result_ty = layout.llvm_type;
            self.zero_result_payload_area(result_ty, src_ptr, "respl.place.move");
        } else {
            self.zero_option_field_tag_at(src_ptr);
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
            let Some(layout) = self.enum_layouts.get("Result") else {
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
            let Some(layout) = self.enum_layouts.get("Result") else {
                return;
            };
            let result_ty = layout.llvm_type;
            self.zero_result_payload_area(result_ty, src_ptr, "respl.assignmove");
        } else {
            self.zero_option_field_tag_at(src_ptr);
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

    /// No arm moves any of its pattern's leaf bindings out of the arm body or
    /// guard (the escape guard shared by the read-only borrow classifications).
    fn no_arm_payload_escapes(&self, arms: &[MatchArm]) -> bool {
        for arm in arms {
            let mut names: Vec<String> = Vec::new();
            collect_pattern_bindings(&arm.pattern, &mut names);
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
    fn borrow_get_payload_clone_te(&mut self, scrutinee: &Expr) -> Option<TypeExpr> {
        // A `let g = m.get(k)` binding re-enters here as an IDENTIFIER scrutinee
        // (`match g { … }` / `if let Some(v) = g`); its recorded `Option[V]`
        // type comes from `borrow_accessor_let_payload`, not the span-keyed
        // `enum_inst_type_exprs` (which keys on the get-CALL span, not the
        // identifier use). B-2026-07-09-13.
        let te = if let ExprKind::Identifier(name) = &scrutinee.kind {
            self.borrow_accessor_let_payload.get(name)?.clone()
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
            self.enum_inst_type_exprs.get(&key)?.clone()
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
                (self.struct_types.contains_key(head) || self.enum_layouts.contains_key(head))
                    && !self.shared_types.contains_key(head)
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
                let Some(field_names) = self.struct_field_names.get(&struct_name).cloned() else {
                    return Ok(());
                };
                let field_tes = self
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
                (self.struct_types.contains_key(head) || self.enum_layouts.contains_key(head))
                    && !self.shared_types.contains_key(head)
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
                    if let Some(frame) = self.scope_cleanup_actions.last_mut() {
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
                let (key_is_vec, val_is_vec, key_shared, val_shared, val_drop_fn) =
                    self.map_temp_cleanup_parts(te);
                self.track_map_var_with_val_drop(
                    slot.ptr,
                    key_is_vec,
                    val_is_vec,
                    val_shared,
                    key_shared,
                    val_drop_fn,
                );
            }
            _ => {
                if self.struct_types.contains_key(head) {
                    let head = head.to_string();
                    self.track_struct_var(&head, slot.ptr);
                } else if self.enum_layouts.contains_key(head) {
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
                self.borrow_binding_escapes(callee, name)
                    || args.iter().any(|a| {
                        if is_print {
                            borrow_pos(self, &a.value)
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
                // A bare-`name` scrutinee could destructure the alias's
                // payload out — conservative move.
                self.borrow_binding_escapes(scrutinee, name)
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
            PatternKind::Struct { path, .. }
                if path.len() > 1
                    || self
                        .enum_tag_for_variant(path.last().map(|s| s.as_str()).unwrap_or(""))
                        .is_some() =>
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
                    span: span.clone(),
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
                        )
                    )
                );
            }
            RangeBound::Path { segments, .. } if segments.len() == 1 => self
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
            for (enum_name, layout) in &self.enum_layouts {
                if layout.tags.contains_key(variant_name) {
                    if let Some(info) = self.shared_types.get(enum_name) {
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

    /// Find the discriminant tag for a variant name across all registered enums.
    /// Prefers user-declared enums over the seeded built-ins (`Option`,
    /// `Result`, `Json`, `TcpError`, …) when the variant name collides
    /// (e.g. user `MyIoErr.Other` vs seeded `TcpError.Other`). Without
    /// the preference, HashMap iteration order picks one at random — the
    /// 2026-05-25 codegen-suite intermittent-hang investigation found
    /// that the match-dispatch site sometimes loaded the wrong enum's
    /// tag, sending all comparisons down the wildcard `_` arm.
    pub(super) fn enum_tag_for_variant(&self, variant_name: &str) -> Option<u64> {
        let mut user_hit: Option<u64> = None;
        let mut seed_hit: Option<u64> = None;
        for (en, layout) in &self.enum_layouts {
            if let Some(&tag) = layout.tags.get(variant_name) {
                if self.seeded_enum_names.contains(en) {
                    seed_hit.get_or_insert(tag);
                } else {
                    user_hit.get_or_insert(tag);
                }
            }
        }
        user_hit.or(seed_hit)
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
            if let Some(layout) = self.enum_layouts.get(qualifier) {
                if layout.tags.contains_key(variant_name) {
                    return Some(qualifier.to_string());
                }
            }
        }
        // #39 — an unqualified variant resolves against the match scrutinee's
        // enum first, so a name shared with another imported enum (`Float` in
        // both `Token` and `Expr`) binds to the scrutinee's variant, not
        // whichever the unordered map yields first.
        if let Some(hint) = &self.match_scrutinee_enum_hint {
            if let Some(layout) = self.enum_layouts.get(hint) {
                if layout.tags.contains_key(variant_name) {
                    return Some(hint.clone());
                }
            }
        }
        let mut user_hit: Option<String> = None;
        let mut seed_hit: Option<String> = None;
        for (en, l) in &self.enum_layouts {
            if l.tags.contains_key(variant_name) {
                if self.seeded_enum_names.contains(en) {
                    seed_hit.get_or_insert_with(|| en.clone());
                } else {
                    user_hit.get_or_insert_with(|| en.clone());
                }
            }
        }
        user_hit.or(seed_hit)
    }

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
            if let Some(layout) = self.enum_layouts.get(segments[segments.len() - 2]) {
                if let Some(&tag) = layout.tags.get(variant_name) {
                    return Some((layout.llvm_type, tag));
                }
            }
        }
        // #39 — prefer the match scrutinee's enum for an unqualified variant,
        // so a name shared across enums (`Token.Float` vs `Expr.Float`) resolves
        // to the scrutinee's tag instead of whichever the unordered map yields.
        if let Some(hint) = &self.match_scrutinee_enum_hint {
            if let Some(layout) = self.enum_layouts.get(hint) {
                if let Some(&tag) = layout.tags.get(variant_name) {
                    return Some((layout.llvm_type, tag));
                }
            }
        }
        // Unqualified fallback: user-vs-seed preference, type + tag from the
        // SAME layout (so a downstream tag compare stays self-consistent).
        let mut user_hit: Option<(StructType<'ctx>, u64)> = None;
        let mut seed_hit: Option<(StructType<'ctx>, u64)> = None;
        for (en, l) in &self.enum_layouts {
            if let Some(&tag) = l.tags.get(variant_name) {
                if self.seeded_enum_names.contains(en) {
                    seed_hit.get_or_insert((l.llvm_type, tag));
                } else {
                    user_hit.get_or_insert((l.llvm_type, tag));
                }
            }
        }
        user_hit.or(seed_hit)
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
        self.enum_layouts
            .iter()
            .find(|(_, l)| {
                l.tags.contains_key(variant_name)
                    && scrut_struct_ty
                        .as_ref()
                        .map(|t| &l.llvm_type == t)
                        .unwrap_or(true)
            })
            .map(|(_, l)| l)
            .or_else(|| {
                let mut user_hit: Option<&super::state::EnumLayout<'ctx>> = None;
                let mut seed_hit: Option<&super::state::EnumLayout<'ctx>> = None;
                for (en, l) in &self.enum_layouts {
                    if l.tags.contains_key(variant_name) {
                        if self.seeded_enum_names.contains(en) {
                            seed_hit.get_or_insert(l);
                        } else {
                            user_hit.get_or_insert(l);
                        }
                    }
                }
                user_hit.or(seed_hit)
            })
            .and_then(|l| l.field_word_offsets.get(variant_name).cloned())
            .unwrap_or_else(|| (0..num_patterns).map(|i| (i, 1)).collect())
    }

    /// AND into `cond` the inner-tag checks for any *variant* sub-pattern
    /// of an outer variant — e.g. the inner `E.A` of `Result.Err(E.A(c))`.
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
        if !sub_patterns
            .iter()
            .any(|p| self.variant_pattern_enum_and_tag(p).is_some())
        {
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
            let Some((_inner_ty, expected_tag)) = self.variant_pattern_enum_and_tag(sub) else {
                continue;
            };
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
                    if self.shared_types.contains_key(&name) {
                        return 1;
                    }
                    if let Some(layout) = self.enum_layouts.get(&name) {
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
                if !self.pattern_binding_types.contains_key(&key) {
                    let variant_name = name.rsplit('.').next().unwrap_or(name);
                    if name.contains('.') || self.enum_tag_for_variant(variant_name).is_some() {
                        if let Some(en) = self.variant_pattern_enum_name(pat) {
                            if self.shared_types.contains_key(&en) {
                                return 1;
                            }
                            if let Some(layout) = self.enum_layouts.get(&en) {
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
                    self.pattern_binding_types.get(&key).map(|s| s.as_str()),
                    Some("Tuple")
                ) {
                    if let Some(te) = self.pattern_binding_inner_types.get(&key) {
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
                if let Some(te) = self.pattern_binding_inner_types.get(&key) {
                    if self.is_generic_named_struct_type_expr(te) {
                        return Self::llvm_type_word_count(self.llvm_type_for_type_expr(te)).max(1);
                    }
                }
                match self.pattern_binding_types.get(&key).map(|s| s.as_str()) {
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
                    // Shared type (struct OR enum): RC heap pointer = exactly one
                    // word. Must precede the struct/enum arms (see the twin note
                    // in `pattern_payload_llvm_type`) — a direct recursive shared
                    // enum field is one pointer word, not the inline tagged-union
                    // size.
                    Some(name) if self.shared_types.contains_key(name) => 1,
                    Some(name) => self
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
                            self.enum_layouts
                                .get(name)
                                .map(|layout| Self::llvm_type_word_count(layout.llvm_type.into()))
                        })
                        .unwrap_or(1),
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
                        .enum_layouts
                        .get(&enum_name)
                        .map(|l| Self::llvm_type_word_count(l.llvm_type.into()))
                        .unwrap_or(1);
                }
                path.last()
                    .and_then(|n| self.struct_types.get(n.as_str()))
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
                    if self.shared_types.contains_key(&name) {
                        return self.context.ptr_type(AddressSpace::default()).into();
                    }
                    if let Some(layout) = self.enum_layouts.get(&name) {
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
                if !self.pattern_binding_types.contains_key(&key) {
                    let variant_name = name.rsplit('.').next().unwrap_or(name);
                    if name.contains('.') || self.enum_tag_for_variant(variant_name).is_some() {
                        if let Some(en) = self.variant_pattern_enum_name(pat) {
                            if self.shared_types.contains_key(&en) {
                                return self.context.ptr_type(AddressSpace::default()).into();
                            }
                            if let Some(layout) = self.enum_layouts.get(&en) {
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
                    self.pattern_binding_types.get(&key).map(|s| s.as_str()),
                    Some("Tuple")
                ) {
                    if let Some(te) = self.pattern_binding_inner_types.get(&key) {
                        if matches!(te.kind, TypeKind::Tuple(_)) {
                            return self.llvm_type_for_type_expr(te);
                        }
                    }
                }
                // Generic user-struct payload binding — the mono aggregate type
                // (sibling of the word-count arm in `pattern_payload_word_count`).
                if let Some(te) = self.pattern_binding_inner_types.get(&key) {
                    if self.is_generic_named_struct_type_expr(te) {
                        return self.llvm_type_for_type_expr(te);
                    }
                }
                match self.pattern_binding_types.get(&key).map(|s| s.as_str()) {
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
                    Some(name) if self.shared_types.contains_key(name) => {
                        self.context.ptr_type(AddressSpace::default()).into()
                    }
                    Some(name) => self
                        .struct_types
                        .get(name)
                        .map(|st| (*st).into())
                        .or_else(|| {
                            // Enum-typed binding: return the enum's tagged-
                            // union LLVM type so `reconstruct_payload_value`
                            // builds a struct of the right shape. Mirrors the
                            // analogous arm in `pattern_payload_word_count`.
                            self.enum_layouts
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
                    if let Some(layout) = self.enum_layouts.get(&enum_name) {
                        return layout.llvm_type.into();
                    }
                }
                path.last()
                    .and_then(|n| self.struct_types.get(n.as_str()))
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
            self.pattern_binding_types
                .get(&key)
                .map(|n| self.struct_types.contains_key(n.as_str()))
                .unwrap_or(false)
                // Struct-pattern destructure (slice 3t): a 1-word struct
                // payload (`struct P { x: i64 }`) must still rebuild the
                // `{i64}` aggregate — the raw-word path would hand the
                // struct bind arm an IntValue its guard rejects.
                || matches!(
                    &sub_pat.kind,
                    PatternKind::Struct { path, .. }
                        if path.last().is_some_and(|n| self.struct_types.contains_key(n.as_str()))
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
            let key = (sub_pat.span.offset, sub_pat.span.length);
            if let Some(name) = self.pattern_binding_types.get(&key).cloned() {
                match self.llvm_type_for_name(&name) {
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
            self.pattern_binding_types.get(&key).map(|s| s.as_str()),
            Some("Tuple")
        ) {
            if let Some(te) = self.pattern_binding_inner_types.get(&key) {
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
        // Multi-word: resolve the binding's surface type to choose the
        // target LLVM aggregate type. A Struct-PATTERN sub-pattern (slice
        // 3t: `Some(Holder { name, id })`) has no `pattern_binding_types`
        // entry at its own span — its name comes from the pattern path, and
        // the existing field-by-field rebuild below (incl. the #44 nested
        // sub-field recursion) then produces the correctly-typed aggregate
        // for `bind_pattern_values`' plain-struct destructure arm.
        let type_name =
            self.pattern_binding_types
                .get(&key)
                .cloned()
                .or_else(|| match &sub_pat.kind {
                    PatternKind::Struct { path, .. } => path
                        .last()
                        .filter(|n| self.struct_types.contains_key(n.as_str()))
                        .cloned(),
                    _ => None,
                });
        // A generic user-struct payload binding rebuilds into its MONO
        // aggregate (the concrete-arg field layout), NOT the all-`i64` generic
        // base `struct_types[name]` the fallthrough would pick — else the
        // 3-word `String` field collapses into the 1-field base (B-2026-07-12-2).
        let mono_struct_target: Option<BasicTypeEnum<'ctx>> = self
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
                            self.enum_layouts
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
    fn reconstruct_struct_from_words(
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
        &self,
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
        let enum_name = match self.var_type_names.get(scrut_name) {
            Some(n) => n.clone(),
            None => return,
        };
        self.suppress_destructured_enum_payload_cleanup_at(slot.ptr, &enum_name, pattern);
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
        if !self.enum_layouts.contains_key(&enum_name) {
            return;
        }
        let Some(field_ptr) = self.field_chain_place_ptr(scrutinee) else {
            return;
        };
        self.suppress_destructured_enum_payload_cleanup_at(field_ptr, &enum_name, pattern);
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
        if self.shared_types.contains_key(&struct_name) {
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
        if self.shared_types.contains_key(struct_name) {
            return;
        }
        let Some(field_type_names) = self.struct_field_type_names.get(struct_name).cloned() else {
            return;
        };
        let Some(field_names) = self.struct_field_names.get(struct_name).cloned() else {
            return;
        };
        let Some(&st) = self.struct_types.get(struct_name) else {
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
                if let Some(layout) = self.enum_layouts.get("Result") {
                    let result_ty = layout.llvm_type;
                    self.zero_result_payload_area(result_ty, field_ptr, "p16.res");
                }
            } else {
                if let Some(layout) = self.enum_layouts.get(fname).cloned() {
                    if !layout.is_shared {
                        self.zero_enum_payload_caps(field_ptr, &layout);
                    }
                } else if self.struct_types.contains_key(fname)
                    && !self.shared_types.contains_key(fname)
                {
                    self.zero_struct_move_caps(field_ptr, fname);
                }
            }
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
                if self.ref_params.contains_key(name.as_str()) {
                    return None;
                }
                self.variables.get(name.as_str()).map(|s| s.ptr)
            }
            ExprKind::SelfValue => {
                if self.ref_params.contains_key("self") {
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
                if !self.vec_elem_types.contains_key(vec_var.as_str())
                    || !matches!(index.kind, ExprKind::Identifier(_) | ExprKind::Integer(..))
                {
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
                let st = *self.struct_types.get(obj_ty.as_str())?;
                let idx = self
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
                let st = *self.struct_types.get(obj_ty.as_str())?;
                let idx = self
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
                    .struct_field_names
                    .get(obj_ty.as_str())?
                    .iter()
                    .position(|n| n == field)?;
                match &self
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
            Some(root) if self.owned_struct_params.contains(root) => return,
            Some(_) => {}
            None => return,
        }
        let Some(elems) = self.place_chain_tuple_tes(object) else {
            return;
        };
        let Some(te) = elems.get(*index as usize).cloned() else {
            return;
        };
        let Some(base_ptr) = self.field_chain_place_ptr(object) else {
            return;
        };
        let Some(tuple_ty) = self.place_chain_aggregate_llvm_type(object) else {
            return;
        };
        self.zero_tuple_elem_cap_at(base_ptr, tuple_ty, *index as u32, &te);
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
                    return match self.var_elem_type_exprs.get(v.as_str()).map(|te| &te.kind) {
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
                        .struct_field_names
                        .get(obj_ty.as_str())?
                        .iter()
                        .position(|n| n == field)?;
                    let field_te = self
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
                    .struct_field_names
                    .get(obj_ty.as_str())?
                    .iter()
                    .position(|n| n == field)?;
                self.struct_field_type_names
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
    pub(super) fn suppress_destructured_enum_payload_cleanup_at(
        &self,
        slot_ptr: PointerValue<'ctx>,
        enum_name: &str,
        pattern: &Pattern,
    ) {
        let layout = match self.enum_layouts.get(enum_name) {
            Some(l) => l.clone(),
            None => return,
        };
        if layout.is_shared {
            return;
        }
        // Both tuple-variant (`V(x)`) and struct-variant (`V { f }`) patterns
        // move payload fields into bindings. The variant name is the path's
        // last segment for either shape.
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
        // Declared-position indices of the payload fields this pattern
        // *consumes* (moves into a binding). Tuple-variant fields are
        // positional; struct-variant fields are named, so each named field is
        // mapped to its declared position via `enum_variant_struct_field_names`
        // — the same field order `field_drop_kinds` / `field_word_offsets` and
        // the struct-variant constructor are keyed on. A field is consumed when
        // its sub-pattern binds (directly or via a nested destructure); a
        // `Wildcard`/literal sub-pattern doesn't claim ownership, so the
        // source's drop must still fire (suppressing it would leak). A
        // struct-variant shorthand field (`{ value }`, `pattern: None`) is a
        // direct binding and always consumes.
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
        let layout = match self.enum_layouts.get(enum_name) {
            Some(l) => l.clone(),
            None => return,
        };
        if !layout.is_shared {
            return;
        }
        let heap_type = match self.shared_types.get(enum_name) {
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
    pub(super) fn suppress_inline_option_payload_cleanup(
        &self,
        scrutinee: &Expr,
        pattern: &Pattern,
    ) {
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return;
        };
        if !self.inline_option_payload_vars.contains(name.as_str()) {
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
        let Some(layout) = self.enum_layouts.get("Option") else {
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
    pub(super) fn suppress_boxed_enum_payload_cleanup_for_moved_arg(&self, arg: &Expr) {
        let ExprKind::Identifier(name) = &arg.kind else {
            return;
        };
        if !self.boxed_enum_payload_vars.contains(name.as_str()) {
            return;
        }
        let Some(slot) = self.variables.get(name.as_str()) else {
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
        if !self.inline_option_payload_vars.contains(name.as_str()) {
            return;
        }
        let Some(slot) = self.variables.get(name.as_str()) else {
            return;
        };
        let Some(layout) = self.enum_layouts.get("Option") else {
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
        if !self.inline_result_payload_vars.contains(name.as_str()) {
            return;
        }
        let Some(slot) = self.variables.get(name.as_str()) else {
            return;
        };
        let Some(layout) = self.enum_layouts.get("Result") else {
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
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return;
        };
        if !self.inline_result_payload_vars.contains(name.as_str()) {
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
        let Some(slot) = self.variables.get(name.as_str()) else {
            return;
        };
        let Some(layout) = self.enum_layouts.get("Result") else {
            return;
        };
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
    pub(super) fn suppress_inline_option_result_binding_move(&self, value: &Expr) {
        let ExprKind::Identifier(name) = &value.kind else {
            return;
        };
        let in_option = self.inline_option_payload_vars.contains(name.as_str());
        let in_result = self.inline_result_payload_vars.contains(name.as_str());
        // `boxed_enum_payload_vars` covers the heap-BOXED wide payload
        // (`Option[Block]`) whose `BoxedEnumDrop` guards on `tag == Some`; the
        // two inline sets cover the inline `{ptr,len,cap}` heap payload whose
        // free guards on `cap > 0`. Zeroing the whole slot below neutralizes
        // every shape's guard at once.
        let in_boxed = self.boxed_enum_payload_vars.contains(name.as_str());
        if !in_option && !in_result && !in_boxed {
            return;
        }
        let Some(slot) = self.variables.get(name.as_str()).copied() else {
            return;
        };
        let layout_name = if in_result { "Result" } else { "Option" };
        let Some(layout) = self.enum_layouts.get(layout_name) else {
            return;
        };
        let _ = self
            .builder
            .build_store(slot.ptr, layout.llvm_type.const_zero());
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
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return;
        };
        if !self.inline_option_map_payload_vars.contains(name.as_str()) {
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
        let Some(layout) = self.enum_layouts.get("Option") else {
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
        if !self.inline_option_agg_payload_vars.contains(name.as_str()) {
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
        if !self.inline_option_agg_payload_vars.contains(name.as_str()) {
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
        let Some(layout) = self.enum_layouts.get("Option") else {
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
    pub(super) fn suppress_boxed_payload_struct_destructure(
        &mut self,
        scrutinee: &Expr,
        pattern: &Pattern,
    ) {
        let ExprKind::Identifier(name) = &scrutinee.kind else {
            return;
        };
        if !self.boxed_enum_payload_vars.contains(name.as_str()) {
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
        let Some(&st) = self.struct_types.get(struct_name.as_str()) else {
            return;
        };
        // Boxed iff the struct's width exceeds the enum's inline payload
        // area — the same predicate the pack side (`coerce_to_payload_words`)
        // and the unpack side (`reconstruct_payload_value`'s debox) use. An
        // inline payload's w0 is NOT a pointer; zeroing through it would
        // corrupt memory.
        let area = if enum_name == "Option" { 3 } else { 5 };
        if Self::llvm_type_word_count(st.into()) <= area {
            return;
        }
        let Some(slot) = self.variables.get(name.as_str()).copied() else {
            return;
        };
        let Some(layout) = self.enum_layouts.get(enum_name) else {
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
                .build_struct_gep(layout.llvm_type, slot.ptr, 1, "boxfld.suppress.w0")
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
        for field_pat in fields {
            // A `field: _` sub-pattern consumes nothing — the box keeps
            // that field.
            if matches!(
                field_pat.pattern.as_ref().map(|p| &p.kind),
                Some(PatternKind::Wildcard)
            ) {
                continue;
            }
            self.zero_struct_field_move_cap(box_ptr, &struct_name, &field_pat.name);
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
        if self.inline_result_payload_vars.contains(name.as_str()) {
            if let Some(layout) = self.enum_layouts.get("Result") {
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
        if self.inline_option_payload_vars.contains(name.as_str()) {
            if let Some(layout) = self.enum_layouts.get("Option") {
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
        if self.inline_option_map_payload_vars.contains(name.as_str()) {
            if let Some(layout) = self.enum_layouts.get("Option") {
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
        if !force && !self.expr_yields_fresh_owned_temp(scrutinee) && !heap_index {
            return None;
        }
        let BasicValueEnum::StructValue(sv) = val else {
            return None;
        };
        let enum_name = self.variant_pattern_enum_name(pattern)?;
        let layout = self.enum_layouts.get(&enum_name)?;
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
        if has_user_drop {
            self.track_user_drop_var(&enum_name, "__freshtemp_enum_scrut", alloca);
        }
        Some((alloca, enum_name))
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
        let elem_te = self.var_elem_type_exprs.get(vec_name.as_str())?.clone();
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

    pub(super) fn track_freshtemp_boxed_enum_scrutinee(
        &mut self,
        scrutinee: &Expr,
        patterns: &[&Pattern],
        val: BasicValueEnum<'ctx>,
    ) {
        if !self.expr_yields_fresh_owned_temp(scrutinee) {
            return;
        }
        let BasicValueEnum::StructValue(sv) = val else {
            return;
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
            if self.pattern_payload_word_count(payload) <= area {
                continue;
            }
            let Some(variant) = path.last().cloned() else {
                continue;
            };
            let llvm_ty = match self.enum_layouts.get(enum_name.as_str()) {
                Some(l) => l.llvm_type,
                None => continue,
            };
            let Some(fn_val) = self.current_fn else {
                return;
            };
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
                return;
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
                        self.pattern_binding_types.get(&pkey).map(|s| s.as_str()),
                        Some("Tuple")
                    ) {
                        if let Some(te) = self.pattern_binding_inner_types.get(&pkey).cloned() {
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
                                    return;
                                }
                            }
                        }
                    }
                }
            }
            let inner_struct_name: Option<String> = match &payload.kind {
                PatternKind::Binding(_) if !scrutinee_is_borrow => {
                    let pkey = (payload.span.offset, payload.span.length);
                    self.pattern_binding_types.get(&pkey).cloned().filter(|n| {
                        self.struct_types.contains_key(n) && !self.shared_types.contains_key(n)
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
            return;
        }
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
            if let Some(tn) = self.pattern_binding_types.get(&key) {
                if let Some(info) = self.shared_types.get(tn) {
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
                                    if let Some(info) = self.shared_types.get(tn) {
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
        let Some(layout) = self.enum_layouts.get("Option") else {
            return;
        };
        let option_ty = layout.llvm_type;
        let some_tag = layout.tags.get("Some").copied().unwrap_or(1);
        let Some(fn_val) = self.current_fn else {
            return;
        };
        let alloca = self.create_entry_alloca(fn_val, "__freshtemp_shared_opt", option_ty.into());
        let _ = self.builder.build_store(alloca, sv);
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
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
        if ok_payload_elem_ty.is_none()
            && err_payload_elem_ty.is_none()
            && ok_payload_struct_drop.is_none()
            && err_payload_struct_drop.is_none()
        {
            return None;
        }
        let layout = self.enum_layouts.get("Result")?;
        let result_ty = layout.llvm_type;
        let ok_tag = layout.tags.get("Ok").copied().unwrap_or(0);
        let err_tag = layout.tags.get("Err").copied().unwrap_or(1);
        let fn_val = self.current_fn?;
        let alloca = self.create_entry_alloca(fn_val, "__freshtemp_inline_res", result_ty.into());
        let _ = self.builder.build_store(alloca, sv);
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
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
                if let Some(tn) = self.pattern_binding_types.get(&key) {
                    return self.struct_types.contains_key(tn.as_str());
                }
            }
            false
        })
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
        let Some(layout) = self.enum_layouts.get("Result") else {
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
        let (llvm_ty, is_shared) = match self.enum_layouts.get(&enum_name) {
            Some(l) => (l.llvm_type, l.is_shared),
            None => return,
        };
        if is_shared {
            return;
        }
        // `None` ⇒ no heap-bearing variant anywhere ⇒ nothing to drop.
        let Some(drop_fn) = self.emit_enum_drop_switch(&enum_name) else {
            return;
        };
        let Some(fn_val) = self.current_fn else {
            return;
        };
        let alloca = self.create_entry_alloca(fn_val, "__whilelet_miss_scrut", llvm_ty.into());
        let _ = self.builder.build_store(alloca, sv);
        self.builder
            .build_call(drop_fn, &[alloca.into()], "")
            .unwrap();
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
fn collect_pattern_bindings(p: &crate::ast::Pattern, out: &mut Vec<String>) {
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
