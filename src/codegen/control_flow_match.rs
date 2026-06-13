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
        // B-track (pattern-arm unbound heap-field drop): a fresh-temp enum
        // scrutinee (`match make() { … }`) has no source `EnumDrop`, so any arm
        // that leaves a heap payload field unbound leaks it. Materialize +
        // `track_enum_var` once here (enum name resolved from any variant arm —
        // all arms share the scrutinee's enum); each arm's per-arm suppression
        // below then zeroes the caps of fields THAT arm moved into bindings.
        // No-op for non-fresh / non-enum / ref scrutinees.
        let freshtemp_enum = if scrut_ref_ptr.is_none() {
            arms.iter()
                .map(|a| &a.pattern)
                .find(|p| self.variant_pattern_enum_name(p).is_some())
                .and_then(|p| self.materialize_freshtemp_enum_scrutinee(scrutinee, p, scrut))
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
        // Detect borrow-returning scrutinees so pattern bindings don't
        // register a `FreeVecBuffer` against a buffer the container still
        // owns. `Map.get` is the canonical case (the returned `Option[V]`
        // aliases the bucket entry's value words); a duplicate cleanup
        // would double-free against the `karac_map_free_with_val_drop_vec`
        // path at function exit.
        let saved_borrow_flag = self.pattern_binding_is_borrow;
        self.pattern_binding_is_borrow = self.scrutinee_is_borrow_call(scrutinee);
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
                }
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
            if scrut_ref_ptr.is_none() {
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
                    self.suppress_inline_option_map_payload_cleanup(scrutinee, &arm.pattern);
                }
                // #15: a struct-FIELD enum scrutinee (`match spanned.tok { … }`).
                // Runs regardless of the identifier/fresh-temp split above —
                // both neutralize only the scrutinee copy, never the enum field
                // in the SOURCE struct, which the owning struct's drop now frees.
                self.suppress_destructured_struct_field_enum_cleanup(scrutinee, &arm.pattern);
            }

            let arm_val = self.compile_tail_final_expr(&arm.body, tail)?;
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

        // Every arm diverged (`return` / `unreachable()` / `todo()` in all of
        // them): no arm branched to `merge_bb`, so it has no predecessors.
        // Terminate it with `unreachable` so the enclosing fn-tail `ret` guard
        // skips emitting `ret <i64 placeholder>` against a non-i64 return type
        // (the gap-d failure class for an all-diverging `match` tail).
        if arm_results.is_empty() {
            self.builder.build_unreachable().unwrap();
            return Ok(self.context.i64_type().const_int(0, false).into());
        }

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
                if let Some(tag) = self.enum_tag_for_variant(variant_name) {
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
                if let Some(tag) = self.enum_tag_for_variant(variant_name) {
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
                if let Some(tag) = self.enum_tag_for_variant(variant_name) {
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
        mut cond: inkwell::values::IntValue<'ctx>,
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
            cond = self.builder.build_and(cond, tag_eq, "ncond.and").unwrap();
            // Deeper nesting: if this variant's own payload contains further
            // variant sub-patterns, recurse against the rebuilt inner value.
            if let PatternKind::TupleVariant { path, patterns } = &sub.kind {
                let inner_variant = path.last().map(|s| s.as_str()).unwrap_or("");
                cond =
                    self.and_in_nested_variant_conditions(inner, inner_variant, patterns, cond)?;
            }
        }
        Ok(cond)
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
            PatternKind::Binding(_) => {
                let key = (pat.span.offset, pat.span.length);
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
                match self.pattern_binding_types.get(&key).map(|s| s.as_str()) {
                    // VecDeque rides Vec's 3-word `{ptr, len, cap}` layout
                    // (B-2026-06-10-3): without it, a VecDeque enum payload
                    // got the 1-word default → malformed value, crash on use.
                    Some("Vec") | Some("VecDeque") | Some("String") => 3,
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
            PatternKind::Binding(_) => {
                let key = (pat.span.offset, pat.span.length);
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
                match self.pattern_binding_types.get(&key).map(|s| s.as_str()) {
                    Some("Vec") | Some("VecDeque") | Some("String") => {
                        self.vec_struct_type().into()
                    }
                    Some("Slice") => self.slice_struct_type().into(),
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
                        if ft == self.context.f64_type() {
                            let f = self.builder.build_bit_cast(w, ft, "pat.f64.bc").unwrap();
                            return Ok(f);
                        } else {
                            let i32_t = self.context.i32_type();
                            let lo = self
                                .builder
                                .build_int_truncate(w, i32_t, "pat.f32.tr")
                                .unwrap();
                            let f = self.builder.build_bit_cast(lo, ft, "pat.f32.bc").unwrap();
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
                    let elem_llvm_tys: Vec<BasicTypeEnum<'ctx>> = elem_tes
                        .iter()
                        .map(|et| self.llvm_type_for_type_expr(et))
                        .collect();
                    let tuple_ty = self.context.struct_type(&elem_llvm_tys, false);
                    let mut agg = tuple_ty.get_undef();
                    let mut cursor = 0usize;
                    for (i, elem_ty) in elem_llvm_tys.iter().enumerate() {
                        let n = Self::llvm_type_word_count(*elem_ty).max(1);
                        let end = (cursor + n).min(field_words.len());
                        let slice = &field_words[cursor..end];
                        // Primitive single-word elements coerce the
                        // word back to the declared LLVM type (int/bool
                        // bit-cast); multi-word elements aren't expected
                        // here but fall back to the first word as a
                        // safety net.
                        let raw = slice
                            .first()
                            .copied()
                            .unwrap_or_else(|| i64_t.const_int(0, false));
                        let elem_val: BasicValueEnum<'ctx> = match *elem_ty {
                            BasicTypeEnum::IntType(it) if it.get_bit_width() != 64 => self
                                .builder
                                .build_int_truncate(raw, it, "tup.elem.tr")
                                .unwrap()
                                .into(),
                            BasicTypeEnum::IntType(_) => raw.into(),
                            // Float tuple element: bitcast the payload word back
                            // to the float (f64 direct; f32 from the low 32
                            // bits). Mirrors the single-word float fix — needed
                            // for any tuple-payload enum carrying a float, incl.
                            // the lexer's `Token::Float(f64, …)`.
                            BasicTypeEnum::FloatType(ft) if ft == self.context.f64_type() => {
                                self.builder.build_bit_cast(raw, ft, "tup.f64.bc").unwrap()
                            }
                            BasicTypeEnum::FloatType(ft) => {
                                let i32_t = self.context.i32_type();
                                let lo = self
                                    .builder
                                    .build_int_truncate(raw, i32_t, "tup.f32.tr")
                                    .unwrap();
                                self.builder.build_bit_cast(lo, ft, "tup.f32.bc").unwrap()
                            }
                            _ => raw.into(),
                        };
                        agg = self
                            .builder
                            .build_insert_value(agg, elem_val, i as u32, "tup.bind.iv")
                            .unwrap()
                            .into_struct_value();
                        cursor = end;
                    }
                    return Ok(agg.into());
                }
            }
        }
        // Multi-word: resolve the binding's surface type to choose the
        // target LLVM aggregate type.
        let type_name = self.pattern_binding_types.get(&key).cloned();
        let target_ty: Option<BasicTypeEnum<'ctx>> =
            type_name.as_ref().and_then(|n| match n.as_str() {
                "String" | "str" | "Vec" | "VecDeque" => Some(self.vec_struct_type().into()),
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
            let field_val: BasicValueEnum<'ctx> = if n == 1 {
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
                        self.builder.build_bit_cast(word, ft, "pl.fc").unwrap()
                    }
                    BasicTypeEnum::PointerType(_) => self
                        .builder
                        .build_int_to_ptr(word, ptr_ty, "pl.itop")
                        .unwrap()
                        .into(),
                    _ => word.into(),
                }
            } else if let BasicTypeEnum::StructType(inner_st) = field_ty {
                // Nested struct field — recursively build by walking its
                // sub-fields. v1 covers the `String` aggregate
                // (`{ ptr, i64, i64 }`) embedded in `Response.body` /
                // `HttpError.message`; deeper nesting would need
                // recursion, but those shapes don't surface here yet.
                let mut sub_agg = inner_st.get_undef();
                let sub_fields = inner_st.count_fields() as usize;
                for j in 0..sub_fields {
                    if j >= slice.len() {
                        break;
                    }
                    let sub_ty = inner_st
                        .get_field_type_at_index(j as u32)
                        .ok_or_else(|| format!("sub-field {} missing", j))?;
                    let sw = slice[j];
                    let sub_val: BasicValueEnum<'ctx> = match sub_ty {
                        BasicTypeEnum::IntType(it) => {
                            if it.get_bit_width() == 64 {
                                sw.into()
                            } else if it.get_bit_width() < 64 {
                                self.builder
                                    .build_int_truncate(sw, it, "pl.sub.tr")
                                    .unwrap()
                                    .into()
                            } else {
                                self.builder
                                    .build_int_z_extend(sw, it, "pl.sub.zx")
                                    .unwrap()
                                    .into()
                            }
                        }
                        BasicTypeEnum::PointerType(_) => self
                            .builder
                            .build_int_to_ptr(sw, ptr_ty, "pl.sub.itop")
                            .unwrap()
                            .into(),
                        BasicTypeEnum::FloatType(ft) => {
                            self.builder.build_bit_cast(sw, ft, "pl.sub.fc").unwrap()
                        }
                        _ => sw.into(),
                    };
                    sub_agg = self
                        .builder
                        .build_insert_value(sub_agg, sub_val, j as u32, "pl.sub.iv")
                        .unwrap()
                        .into_struct_value();
                }
                sub_agg.into()
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
        let scrut_name = match &scrutinee.kind {
            ExprKind::Identifier(n) => n,
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
        let ExprKind::FieldAccess { .. } = &scrutinee.kind else {
            return;
        };
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
            ExprKind::Identifier(name) => self.variables.get(name.as_str()).map(|s| s.ptr),
            ExprKind::SelfValue => self.variables.get("self").map(|s| s.ptr),
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
            _ => None,
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
                let ExprKind::Identifier(v) = &object.kind else {
                    return None;
                };
                match self.var_elem_type_exprs.get(v.as_str()).map(|te| &te.kind) {
                    Some(TypeKind::Path(p)) => p.segments.last().cloned(),
                    _ => None,
                }
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
        let (path, sub_patterns) = match &pattern.kind {
            PatternKind::TupleVariant { path, patterns } => (path, patterns),
            _ => return,
        };
        let variant_name = match path.last() {
            Some(n) => n.as_str(),
            None => return,
        };
        let drop_kinds = match layout.field_drop_kinds.get(variant_name) {
            Some(k) => k,
            None => return,
        };
        let offsets = match layout.field_word_offsets.get(variant_name) {
            Some(o) => o,
            None => return,
        };
        let i64_t = self.context.i64_type();
        let zero = i64_t.const_int(0, false);
        for ((sub_pat, kind), (start_word, num_words)) in sub_patterns
            .iter()
            .zip(drop_kinds.iter())
            .zip(offsets.iter())
        {
            // Suppression only fires when the sub-pattern *consumes*
            // the payload field — i.e. binds it to a name (directly or
            // via a nested destructure). A `Wildcard` or literal
            // pattern doesn't claim ownership, so the source's drop
            // *should* fire to free the payload; suppressing those
            // would leak. Nested `Tuple` patterns inside a payload
            // (e.g. `V((xs, s, n))`) consume the field when any inner
            // binding claims part of it — the inner cleanup will free
            // the whole composite, so the outer source's drop must
            // still be skipped.
            if !pattern_consumes_field(sub_pat) || *kind != super::state::EnumDropKind::VecOrString
            {
                continue;
            }
            // Cap word for a 3-word Vec/String payload (data, len, cap)
            // is at LLVM struct index `1 (tag) + start_word + num_words - 1`
            // = `start_word + num_words`. The DP1 lock pins `num_words == 3`
            // for `VecOrString`, but we compute from `num_words` rather
            // than hard-coding 3 so the helper stays correct if the
            // layout ever grows additional words.
            let cap_index = (start_word + num_words) as u32;
            if let Ok(cap_ptr) = self.builder.build_struct_gep(
                layout.llvm_type,
                slot_ptr,
                cap_index,
                "match.dest.cap.suppress.p",
            ) {
                let _ = self.builder.build_store(cap_ptr, zero);
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
        let i64_t = self.context.i64_type();
        if let Ok(cap_ptr) =
            self.builder
                .build_struct_gep(layout.llvm_type, slot.ptr, 3, "respl.suppress.cap")
        {
            let _ = self.builder.build_store(cap_ptr, i64_t.const_int(0, false));
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
    ) -> Option<(PointerValue<'ctx>, String)> {
        if !self.expr_yields_fresh_owned_temp(scrutinee) {
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
        // Only materialize when some variant actually has a heap-bearing
        // payload to drop — otherwise `track_enum_var` is a no-op (and
        // `emit_enum_drop_switch` returns None), so the alloca would be dead.
        let has_droppable = layout
            .field_drop_kinds
            .values()
            .any(|ks| ks.iter().any(|k| *k != super::state::EnumDropKind::None));
        if !has_droppable {
            return None;
        }
        let llvm_ty = layout.llvm_type;
        let fn_val = self.current_fn?;
        let alloca = self.create_entry_alloca(fn_val, "__freshtemp_enum_scrut", llvm_ty.into());
        let _ = self.builder.build_store(alloca, sv);
        self.track_enum_var(&enum_name, alloca);
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
            let inner_struct_name: Option<String> = match &payload.kind {
                PatternKind::Binding(_) => {
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
    pub(super) fn drop_freshtemp_enum_scrutinee_on_miss(
        &mut self,
        scrutinee: &Expr,
        pattern: &Pattern,
        val: BasicValueEnum<'ctx>,
    ) {
        if !self.expr_yields_fresh_owned_temp(scrutinee) {
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
