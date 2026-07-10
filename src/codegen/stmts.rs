//! Statement, block, and function-body compilation.
//!
//! Houses `compile_block`, `compile_function_body` (with its
//! parallel-group dispatch), `compile_stmt` (the per-stmt match
//! driver), the `stmt_is_par_block` recogniser, the
//! `compute_return_slots_checked` slot-allocation pass, the
//! `infer_let_binding_llvm_type` heuristic, and `bind_pattern`
//! (the top-level pattern destructuring).

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::concurrency::ParallelGroup;

use inkwell::module::Linkage;
use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::{BasicValue, BasicValueEnum, GlobalValue, PointerValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

use super::helpers::{
    map_kv_type_exprs, set_inner_type_expr, slice_inner_type_expr, vec_inner_type_expr,
};
use super::state::{B2Role, ReturnSlot, SharedTypeInfo, VarSlot};

impl<'ctx> super::Codegen<'ctx> {
    pub(super) fn compile_block(
        &mut self,
        block: &Block,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // Consume the tail-return context up front so block STATEMENTS compile
        // with it cleared (a non-tail `if let` in stmt position must not pick up
        // tail-return compensation); restore it for the final expr below.
        let tail_inner = self.tail_ret_inner.take();
        for stmt in &block.stmts {
            self.compile_stmt(stmt)?;
            if self
                .builder
                .get_insert_block()
                .unwrap()
                .get_terminator()
                .is_some()
            {
                return Ok(None);
            }
        }
        if let Some(ref expr) = block.final_expr {
            let val = self.compile_tail_final_expr(expr, tail_inner)?;
            Ok(Some(val))
        } else {
            Ok(None)
        }
    }

    /// Compile a block's final expression, applying per-branch tail-return
    /// compensation for `Option[shared T]` returns when `tail_inner` is `Some`
    /// (i.e. this block's value IS the function's return value).
    ///
    /// - A bare `Option[shared]` binding leaf (`l1`) is inc'd in THIS block —
    ///   returning the binding moves it out, but its scope-exit `RcDecOption`
    ///   still fires, so the returned chain needs +1. Emitting it here (in the
    ///   specific arm that returns it) is the per-branch compensation that lets
    ///   a function mix `Some(<alias>)` tails (no inc) with bare-arg tails.
    /// - `if` / `if let` / `match` / block constructs PROPAGATE the context to
    ///   their own branch finals (re-set `tail_ret_inner`), so the inc lands in
    ///   the deepest arm that actually returns a bare binding.
    /// - A `var.field` leaf takes the loaded-inner inc via
    ///   `share_option_shared_field_ref_for_arg` (binding-rooted objects only;
    ///   call-like objects already inc in `compile_field_access`'s call-chain
    ///   branch).
    /// - `Some(...)` / a call move-out / anything else get no inc (the
    ///   constructor already inc'd a `Some` payload; a call owns its ref).
    pub(super) fn compile_tail_final_expr(
        &mut self,
        expr: &Expr,
        tail_inner: Option<StructType<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Chained borrow return (`-> ref T` fn whose tail is `echo(t)`): admit
        // the borrow-returning call as the one sanctioned site (bypass the
        // direct-use gate in `compile_call`). The result is the borrow `ptr`,
        // which `compile_function` returns directly via `is_borrow_returning_
        // call_expr`. A `-> ref T` fn never carries an `Option[shared]`
        // tail-inner, so this precedes (and is disjoint from) the inc logic.
        if self.current_fn_returns_ref && self.is_borrow_returning_call_expr(expr) {
            let prev = self.compiling_ref_return_let_rhs;
            self.compiling_ref_return_let_rhs = true;
            let v = self.compile_expr(expr);
            self.compiling_ref_return_let_rhs = prev;
            return v;
        }
        // Tail-CALL SoA return propagation: in a SoA-returning monomorph whose
        // body (or a tail branch) ENDS IN a layout-returning call — `substep`'s
        // `fan_stream(coll, …)` — flow this function's return layout to that call
        // so it is return-SoA monomorphized and yields the 4-field struct this
        // function returns. The tail-IDENTIFIER case (`init_grid`'s trailing
        // `grid`) is handled by `soa_return_local_names` seeding the local; this
        // is its tail-CALL analog. Gated on no nearer context having already
        // parked a layout (a `let`/assign RHS sets `pending_return_layout`
        // itself) and on the callee actually returning a `Vec[E]`
        // (`let_rhs_calls_layout_returning_fn` matches only a direct call, so
        // `If`/`Match`/`Block` tails fall through to their own branch finals
        // below and re-enter here per branch). Consumed by `compile_call`.
        if matches!(self.return_layout, super::state::LayoutId::Soa(_))
            && self.pending_return_layout.is_none()
            && self.let_rhs_calls_layout_returning_fn(expr)
        {
            self.pending_return_layout = Some(self.return_layout.clone());
        }
        let Some(inner) = tail_inner else {
            return self.compile_expr(expr);
        };
        match &expr.kind {
            ExprKind::If { .. }
            | ExprKind::IfLet { .. }
            | ExprKind::Match { .. }
            | ExprKind::Block(_)
            | ExprKind::Unsafe(_)
            | ExprKind::LabeledBlock { .. } => {
                // Re-arm the context for the construct's branch finals.
                self.tail_ret_inner = Some(inner);
                let v = self.compile_expr(expr)?;
                self.tail_ret_inner = None;
                Ok(v)
            }
            ExprKind::Identifier(n) if self.var_option_shared_heap.contains_key(n.as_str()) => {
                let v = self.compile_expr(expr)?;
                self.share_option_shared_ref_for_arg(expr);
                Ok(v)
            }
            // Tail field return (`fn f(...) -> Option[T] { x.next }`):
            // the returned alias takes its own +1 via the loaded-inner
            // inc — `share_option_shared_field_ref_for_arg` self-gates on
            // "Identifier/self-bound shared object with an
            // `Option[shared T]` field" and no-ops otherwise. Uniform for
            // every root kind:
            //   - borrowed (`ref self` / `ref T` param): the object
            //     belongs to the caller; the alias +1 is balanced by
            //     whatever ownership the caller registers for the result.
            //   - owned shared param (`self` / `node`): the callee's
            //     entry receive-inc + scope-exit dec keep the object
            //     alive through the frame; the alias +1 survives it.
            //   - owned local (`dummy.next`, the kata-#2 shape): the
            //     inc (+1) and the dying owner's recursive-drop dec (-1)
            //     net to a wholesale transfer of the field's ref.
            // This replaced the move-out tail ZEROING
            // (`suppress_tail_field_option_dec`, retired 2026-06-05):
            // zeroing mutated the heap object — wrong whenever any other
            // ref could observe it (owned-shared `self` with the caller
            // still holding the receiver severed the caller's list), and
            // its ref-root addressing wrote through the un-deref'd param
            // slot into the caller's stack frame.
            ExprKind::FieldAccess { object, field } => {
                let v = self.compile_expr(expr)?;
                // C1b RootLink: `<root>.<link>` at fn tail is the
                // sanctioned structural transfer — the b2 count-free
                // build left every chain node at rc==1 straight from
                // rc_alloc, the root's cleanup frees ONLY the header
                // node, and the loaded link carries the chain out
                // owning that rc==1. The compensating loaded-inner inc
                // below would inflate the transfer to rc==2 (leak).
                let structural_transfer = matches!(&object.kind, ExprKind::Identifier(n)
                if self.cluster_root_info(n).is_some_and(|(member, link_idx, mode)| {
                    mode == crate::ownership::ReturnedChain::RootLink
                        && self
                            .struct_field_names
                            .get(&member)
                            .and_then(|ns| ns.get(link_idx))
                            .is_some_and(|ln| ln == field)
                }));
                if !structural_transfer {
                    self.share_option_shared_field_ref_for_arg(expr, v);
                }
                Ok(v)
            }
            _ => self.compile_expr(expr),
        }
    }

    /// Slice 1.5 (Phase 7 defer codegen). Compile a "naked" block —
    /// one whose enclosing construct does NOT already manage a
    /// `scope_cleanup_actions` frame (if/if-let arms, bare
    /// `{ ... }` / `Seq` / `unsafe` expression-blocks, nested
    /// `defer` bodies). Pushes a fresh frame at entry; on normal
    /// fall-through drains it via `drain_top_frame_with_emit`
    /// (emitting the cleanup IR inside the block's current BB);
    /// on early-terminator paths the frame was already walked by
    /// the early-exit's `emit_scope_cleanup`, so we just pop.
    ///
    /// Block-scoping a frame closes two slice-1 gaps in one
    /// shape:
    /// 1. **Block-scope dispatch** — a `defer` inside the block
    ///    fires at block exit, not function exit (matches the
    ///    interpreter's per-block `cleanup` Vec drain semantics).
    /// 2. **Runtime-reachability** — the drain IR is emitted
    ///    inside the block's BB, so an unreached arm
    ///    (`if false { defer ... }`) never executes the drain.
    ///
    /// Callers that already manage their own frame at the right
    /// scope boundary (`compile_for_range` / `compile_while` /
    /// `compile_loop` / match arms / par-branch worker fns /
    /// `compile_function`) keep using plain `compile_block` and
    /// continue to push+drain at the right granularity.
    pub(super) fn compile_block_with_frame(
        &mut self,
        block: &Block,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        self.scope_cleanup_actions.push(Vec::new());
        let result = self.compile_block(block)?;
        let body_has_terminator = self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_some();
        if !body_has_terminator {
            // The block is used AS A VALUE (`let s = { …; tail }`, an
            // `if`/`match` arm, a call argument): its tail's heap buffer is
            // loaded into `result` and escapes to the consumer, which becomes
            // the buffer's owner (the let binding's own `track_vec_var`, the
            // match result, …). Neutralize the tail value's cleanup BEFORE
            // this frame drains so it isn't freed between the tail-value load
            // and the value escaping (a use-after-free) and isn't double-freed
            // against the consumer's owner cleanup (B-2026-06-11-2). Exactly
            // the move-aware tail handling `compile_function` applies to a
            // function's tail return.
            if let Some(tail) = block.final_expr.as_deref() {
                self.suppress_block_tail_cleanup(tail);
            }
            self.drain_top_frame_with_emit();
        } else {
            self.scope_cleanup_actions.pop();
        }
        Ok(result)
    }

    /// Suppress the cleanup of a value-position block's tail expression before
    /// its frame drains, so the escaping value survives to its consumer (which
    /// owns it). Mirrors `compile_function`'s tail-return suppression:
    ///   - an **f-string** tail zeroes the accumulator's `cap`
    ///     (`zero_vec_alloca_cap`) so the queued `FreeVecBuffer` no-ops;
    ///   - an **identifier** Vec/String/Map/struct tail routes through
    ///     `suppress_source_vec_cleanup_for_arg` (the same move-out suppressor
    ///     the `let b = a;` and tail-return paths use);
    ///   - a **nested block / unsafe** tail recurses to its own tail.
    ///
    /// Without this, `let s = { …; tail }` (and the `if`/`match`-arm and
    /// call-arg block shapes) freed the tail buffer at block-frame drain — a
    /// use-after-free that printed empty (B-2026-06-11-2). The consumer's
    /// binding remains the sole owner: it was loaded with the real `cap` before
    /// the source's `cap` is zeroed here, and an `if`/`match` arm suppresses its
    /// OWN tail per-arm, so a never-run arm's already-zero (entry-init'd) `cap`
    /// stays harmless. A transient/fresh tail (concat, call result) has no
    /// frame-registered cleanup, so this is a no-op there.
    fn suppress_block_tail_cleanup(&mut self, tail: &Expr) {
        match &tail.kind {
            ExprKind::InterpolatedStringLit(_) => {
                if let Some(acc) = self.last_fstr_acc {
                    self.zero_vec_alloca_cap(acc);
                }
            }
            ExprKind::Identifier(_) => {
                self.suppress_source_vec_cleanup_for_arg(tail);
            }
            ExprKind::Block(b) | ExprKind::Seq(b) | ExprKind::Unsafe(b) => {
                if let Some(inner) = b.final_expr.as_deref() {
                    self.suppress_block_tail_cleanup(inner);
                }
            }
            _ => {}
        }
    }

    /// Compile a function's top-level body, dispatching inferred parallel
    /// groups to `karac_par_run` (slice 2 — auto-par codegen MVP).
    ///
    /// Mirrors `compile_block` for the no-analysis path; on top of that,
    /// when the concurrency analysis identifies non-trivial parallel
    /// groups for the current function, the matching contiguous-or-not
    /// stmt sets are batched into a single `emit_par_run` call instead of
    /// being emitted sequentially. Trivial groups (per `is_trivial`) are
    /// skipped — their statements still emit sequentially. This is the
    /// only call site that consumes `parallel_groups_for_current_fn`;
    /// nested blocks (let-RHS, if-arms, loop bodies) keep flowing through
    /// plain `compile_block` because the analyzer's stmt indices only
    /// reference `func.body.stmts`.
    ///
    /// Hard-stop trigger 2 mitigation: a top-level `par {}` stmt has its
    /// inner effects collected by the analyzer (`collect_block_effects`
    /// in `concurrency.rs`), so an effectful par-block already serializes
    /// against neighbors. To stay defensive against pure par-block stmts
    /// being grouped, we drop any group that contains a par-block stmt —
    /// re-parallelizing an already-parallel block would be wasteful at
    /// best and semantically wrong at worst.
    #[allow(clippy::result_large_err)]
    pub(super) fn compile_function_body(
        &mut self,
        body: &Block,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // Slice 6 (Parallax-lite workload): `KARAC_AUTO_PAR=0` flips
        // `auto_par_disabled` on, short-circuiting all parallel-group
        // dispatch back to plain sequential `compile_block`. This is
        // the gate for side-by-side wall-clock benchmarking of auto-par
        // vs sequential codegen on the same workload. The default
        // (auto-par on) is unchanged — gate-on programs continue to
        // hit the parallel-group dispatch path below.
        if self.auto_par_disabled {
            return self.compile_block(body);
        }

        // A2: a coroutine-compiled function body must NOT be auto-parallelized.
        // `coro_ctx` is `Some` exactly while emitting inside a coroutine (set by
        // `emit_coro_ramp` at function entry). Auto-par lifts statement groups
        // into separate `__par_branch_*` worker functions run via
        // `karac_par_run`; a network park inside such a branch would emit its
        // `coro.suspend` + frame-`%hdl` references into the branch function
        // while `coro.begin` lives in the outer ramp — an invalid cross-function
        // reference ("basic block in another function" / "does not dominate")
        // that fails module verification. Semantically a coroutine owns its
        // frame and suspends on the dispatcher; its body can't be sharded onto
        // pool workers. Fall back to sequential `compile_block` for the body.
        if self.coro_ctx.is_some() {
            return self.compile_block(body);
        }

        // Snapshot the analysis up front to release the borrow on `self`
        // before the loop calls `&mut self` methods (`compile_stmt`,
        // `emit_par_run`). The clone is cheap — `ParallelGroup` holds a
        // small `Vec<usize>`, a short `String` reason, and a bool.
        let decision = self.parallel_groups_for_current_fn().cloned();

        let Some(decision) = decision else {
            return self.compile_block(body);
        };

        // The auto-par emission path below compiles statements and the final
        // expr through its own machinery (not `compile_block`). Park the
        // tail-return context in a local and clear the field so a non-tail
        // `if let` in STATEMENT position doesn't pick it up; re-apply it to the
        // final expr via `compile_tail_final_expr` at the tail emission below.
        let auto_par_tail = self.tail_ret_inner.take();

        // Auto-par reduction diagnostic (slice 3a / 3b, 2026-05-19). When
        // the env var `KARAC_REDUCE_DEBUG=1` is set, print every
        // recognized loop reduction discovered for the current function.
        // The diagnostic prints whether the recognized loop *was actually
        // lowered* — slice 3b v1's allow-list is narrow (`+` op, i64
        // accumulator, `for k in 0..hi` shape with no early exits), so
        // recognized reductions outside that set silently fall back to
        // sequential codegen. The diagnostic surfaces that gap so
        // perf-driven follow-ups know which (op, type, shape) to teach
        // next. Pretty-printer is one line per reduction so a tail of
        // build output stays skimmable.
        if std::env::var("KARAC_REDUCE_DEBUG").as_deref() == Ok("1") {
            for r in &decision.loop_reductions {
                eprintln!(
                    "karac-reduce-debug: fn={} stmt_index={} line={} op={} accumulator={}",
                    self.current_fn_name,
                    r.stmt_index,
                    r.loop_line,
                    r.op.symbol(),
                    r.accumulator,
                );
            }
        }

        // Defensive guard: the analyzer walks `func.body.stmts` directly,
        // so its indices should always be in bounds. A `debug_assert!`
        // catches future drift between the analysis and codegen views of
        // the function body without paying the cost in release builds.
        let n = body.stmts.len();
        debug_assert!(
            decision
                .parallel_groups
                .iter()
                .all(|g| g.statement_indices.iter().all(|&i| i < n)),
            "parallel_groups statement_indices out of bounds for function body (len={n})"
        );

        // Build group-start and covered-index lookups. Trivial groups
        // (per the granularity heuristic) are skipped — their stmts emit
        // sequentially as if no group existed. Groups containing an
        // explicit `par {}` stmt are also skipped (hard-stop trigger 2
        // mitigation: don't re-parallelize an already-parallel block).
        //
        // Slice A (Phase-7 — Par codegen: return values, 2026-05-09):
        // groups that define a binding consumed *outside* the group are
        // no longer dropped; instead `compute_return_slots` materializes
        // a per-group `Vec<ReturnSlot>` and `emit_par_run` synthesizes a
        // parent-allocated return struct that branches write into and
        // the parent reads back after `karac_par_run` joins. Empty-slot
        // groups (the parallax-lite shape — three `writes(R_i)` with no
        // captured binding read outside) preserve the slice-2 behavior
        // exactly: empty `Vec<ReturnSlot>` flows through the same path
        // and emits byte-equivalent IR (modulo the spawn-site IDs
        // already minted per group).
        let mut group_starts: HashMap<usize, (&ParallelGroup, Vec<ReturnSlot<'ctx>>)> =
            HashMap::new();
        let mut covered: HashSet<usize> = HashSet::new();
        for group in &decision.parallel_groups {
            if group.is_trivial {
                continue;
            }
            if group
                .statement_indices
                .iter()
                .any(|&i| i < n && Self::stmt_is_par_block(&body.stmts[i]))
            {
                continue;
            }
            // Codegen-time cost gate for the `karac_par_run` path
            // (surfaced 2026-05-23 by the kata-2 bench). The analyzer's
            // `is_trivial` check fires only when `all_pure ||
            // non_constant_count <= 1` — so a 2-stmt group of two
            // independent effectful calls with small per-branch work
            // (kata-2's `let b = make_nines(n); let l1 =
            // from_array(...);`) sails through with `is_trivial = false`
            // even when each branch's resolved cost is well below the
            // dispatch overhead. Without this gate the binary linked
            // ~263 KiB of par-machinery for zero wall-time benefit.
            //
            // Two thresholds, both must clear for the gate to fire:
            //   (a) total < PAR_RUN_DISPATCH_THRESHOLD_UNITS (500) —
            //       the sum of estimated per-branch work is below the
            //       dispatch break-even.
            //   (b) min_per_branch >= PAR_RUN_VISIBILITY_THRESHOLD_UNITS
            //       (50) — every branch has enough resolved structure
            //       for the estimator to be confident. Thin
            //       wrapper-fn-with-method-call branches (parallax's
            //       `fn fetch_profile(uid) { UserDB.fetch_profile(uid) }`
            //       shape, body cost ≈ 10) fall below this floor and
            //       skip gating — their actual work lives inside the
            //       impl method body which the estimator can't see,
            //       and gating them would silently kill real
            //       parallelism wins.
            //
            // The analyzer's per-group `is_trivial` stays unchanged —
            // analyzer tests at tests/concurrency.rs:660-665 + 691-694
            // (which assert 2-effectful-stmt groups are non-trivial)
            // keep passing because the codegen-side gate is a separate
            // skip condition, not a mutation of the analyzer's result.
            //
            // See docs/implementation_checklist/phase-7-codegen.md §
            // "Auto-par `karac_par_run` (find_parallel_groups):
            // per-stmt cost gate" for the design context.
            let group_stmts: Vec<&Stmt> = group
                .statement_indices
                .iter()
                .filter(|&&i| i < n)
                .map(|&i| &body.stmts[i])
                .collect();
            let (total_cost, min_per_branch) = super::reduce::estimate_par_run_group_cost_units(
                self.program_snapshot.as_deref(),
                &group_stmts,
            );
            if total_cost > 0
                && total_cost < super::reduce::PAR_RUN_DISPATCH_THRESHOLD_UNITS
                && min_per_branch >= super::reduce::PAR_RUN_VISIBILITY_THRESHOLD_UNITS
            {
                continue;
            }
            let Some(&min_idx) = group.statement_indices.iter().min() else {
                continue;
            };
            // Drop the group when any binding read outside has an
            // un-typeable RHS — emitting it without that binding's
            // return slot produces "Undefined variable" at the
            // later read site. Sequential fallback is correct
            // (the analyzer's parallelization is an optimization
            // hint, not a semantic requirement).
            let Some(slots) = self.compute_return_slots_checked(group, body) else {
                continue;
            };
            group_starts.insert(min_idx, (group, slots));
            for &i in &group.statement_indices {
                covered.insert(i);
            }
        }

        let mut i = 0;
        while i < n {
            if let Some((group, return_slots)) = group_starts.get(&i).cloned() {
                let group_stmts: Vec<Stmt> = group
                    .statement_indices
                    .iter()
                    .map(|&s| body.stmts[s].clone())
                    .collect();
                // Slice 3 (sub-step d.1): pass a per-group span so the
                // SpawnSiteRecord pinned by `emit_par_run`'s call to
                // `record_spawn_site` carries the location of the first
                // grouped stmt — the conceptual fire-point of the
                // inferred `par_run` — rather than the whole function-
                // body span (slice 2's MVP).
                let group_span = body.stmts[group.statement_indices[0]].span.clone();
                // Slice 1a (Phase 7 — Par codegen: cancellation and
                // error propagation, 2026-05-18) — auto-par dispatch
                // doesn't currently surface Result-typed branches, so
                // the per-branch Result-slot list is empty here. When
                // slice 2 wires inferred Result types through the
                // typechecker, this site folds in alongside.
                // Slice 1b / 2 (2026-05-20 / 2026-05-21) widened
                // `emit_par_run`'s return type to expose the parent-
                // side Result surface (slot array pointer, slot
                // struct type, earliest-err-idx cell pointer); auto-
                // par never has Result-typed branches in slice 1, so
                // the second tuple element is always `None` here and
                // we discard it.
                let (slot_values, _, slot_ownership) =
                    self.emit_par_run(&group_stmts, &group_span, &return_slots, &[])?;
                // Slice A (sub-step g): bind each loaded slot value as a
                // fresh let-binding in the surrounding function-body
                // scope so subsequent stmts referencing the slot's
                // binding-name resolve through the parent's variables
                // table just like any other in-scope local. For owned
                // heap-bearing slot types (Vec / String — same {ptr,
                // len, cap} layout) we register the parent alloca for
                // scope-exit `track_vec_var` cleanup so the moved-in
                // buffer is freed exactly once at the end of the
                // surrounding function body. The branch's
                // `scope_cleanup_actions` are discarded on
                // `emit_par_branch_fn` exit, so the branch alloca is
                // a stranded view of the same bytes — no double-free
                // risk (decision iii: move-only slot semantics with
                // the parent as unique owner).
                if let Some(parent_fn) = self.current_fn {
                    let vec_st: BasicTypeEnum<'ctx> = self.vec_struct_type().into();
                    for slot in &return_slots {
                        if let Some(loaded) = slot_values.get(&slot.binding_name) {
                            let alloca = self.create_entry_alloca(
                                parent_fn,
                                &slot.binding_name,
                                slot.llvm_ty,
                            );
                            self.builder.build_store(alloca, *loaded).unwrap();
                            self.variables.insert(
                                slot.binding_name.clone(),
                                VarSlot {
                                    ptr: alloca,
                                    ty: slot.llvm_ty,
                                },
                            );
                            // Re-register the binding's surface type name so a
                            // narrow *unsigned* slot (`let u: u8 = ...`) keeps
                            // its signedness across the par-group join — the
                            // llvm_ty (`i8`) erases it, and without this
                            // `expr_is_unsigned_int` falls back to signed and
                            // prints `255u8` as `-1` (B-2026-07-03-21).
                            if let Some(tn) = &slot.var_type_name {
                                self.record_var_type_name(slot.binding_name.clone(), tn.clone());
                            }
                            if slot.llvm_ty == vec_st {
                                // Vec/String slot — register a placeholder
                                // i64 element type (matches the
                                // `is_runtime_introspection_call` shape
                                // already in compile_stmt) ONLY if no
                                // entry exists. Without the `or_insert`
                                // guard, an existing element-type
                                // registration from the let-statement's
                                // annotation (e.g. `let v: Vec[bool] =
                                // Vec.filled(...)`) inside the par-group
                                // would be overwritten with i64, breaking
                                // downstream indexed reads / writes
                                // ("PHI node operands are not the same
                                // type as the result" on `not v[i]`
                                // shapes). First-class-T-aware ops still
                                // require an annotation; this preserves
                                // it when present.
                                self.vec_elem_types
                                    .entry(slot.binding_name.clone())
                                    .or_insert_with(|| self.context.i64_type().into());
                                // Track the parent alloca for scope-exit
                                // free. The slot's heap buffer was
                                // allocated inside the branch fn and the
                                // branch's `{ptr, len, cap}` struct was
                                // copied into the parent's return-struct
                                // field; the parent's alloca now points
                                // at the same heap data. Without this
                                // track, the buffer leaks at parent
                                // scope-exit (one Vec / String per slot
                                // per parent invocation — the kata-6
                                // bench at K = 10_000 measured ~474 MiB
                                // peak RSS from this leak alone before
                                // the fix).
                                //
                                // The earlier comment here described a
                                // SIGABRT in the `Holder { items: a, ...
                                // }`-followed-by-`return` demo shape:
                                // re-tracking caused a double-free when
                                // the slot value was moved into a
                                // returned struct field, because the
                                // struct-init copied `{ptr, len, cap}`
                                // verbatim without zeroing the source's
                                // cap. That move-into-struct path is the
                                // dual of the function-tail Identifier
                                // return case `suppress_cleanup_for_tail_return`
                                // already handles via `zero_vec_alloca_cap`,
                                // and the slot value passed as a free-
                                // fn arg case that `suppress_source_vec_cleanup_for_arg`
                                // handles. The right shape for the
                                // struct-field-init dual is the same
                                // (zero the source's cap at the field-
                                // init site so its scope-exit cleanup
                                // no-ops), and lives in
                                // `compile_struct_init` — tracked as a
                                // follow-up "struct-init move suppression
                                // for slot-sourced Vec/String fields".
                                // Until that lands, code that
                                // immediately moves a slot binding into
                                // a struct field then returns the
                                // struct will double-free; the
                                // alternative (leaving the leak) is the
                                // larger pain (482 MiB on the kata-6
                                // bench vs a one-shape SIGABRT no test
                                // exercises today).
                                // Look up the element type we just made
                                // sure exists in `vec_elem_types` (the
                                // or_insert above) so the recursive-drop
                                // fast path inside the `FreeVecBuffer`
                                // emitter has the right element type for
                                // Vec[Vec[T]] / Vec[String] slots.
                                let elem_ty =
                                    self.vec_elem_types.get(slot.binding_name.as_str()).copied();
                                // B-2026-07-02-4: mirror the LET-site cleanup
                                // dispatch (stmts.rs let path). The plain
                                // `track_vec_var` re-track silently DOWNGRADED
                                // a rich element cleanup to the one-level
                                // buffer free — a `Vec[Vec[String]]` slot
                                // crossing the par boundary lost its
                                // `karac_drop_Vec_String` agg drop and leaked
                                // every nested string (auto-par-only: the
                                // KARAC_AUTO_PAR=0 build was clean, which is
                                // how the class hid behind "index-read leak"
                                // shapes — the reads merely changed the
                                // dependency graph enough to parallelize).
                                let elem_te = self
                                    .var_elem_type_exprs
                                    .get(slot.binding_name.as_str())
                                    .cloned();
                                let is_tensor_elem = elem_te
                                    .as_ref()
                                    .map(|te| self.tensor_var_info_from_type_expr(te).is_some())
                                    .unwrap_or(false);
                                let map_elem_drop = elem_te
                                    .as_ref()
                                    .and_then(|te| self.vec_elem_map_drop_for_type_expr(te));
                                let agg_elem_drop = elem_te
                                    .as_ref()
                                    .and_then(|te| self.vec_elem_agg_drop_for_type_expr(te));
                                let is_heap_env_vec = self
                                    .heap_env_vec_owners
                                    .contains(slot.binding_name.as_str());
                                if is_heap_env_vec {
                                    let drop_fn = self.emit_vec_elem_closure_env_drop_fn();
                                    if let Some(et) = elem_ty {
                                        self.track_vec_of_aggs_var(alloca, et, drop_fn);
                                    }
                                } else if is_tensor_elem {
                                    self.track_vec_of_tensors_var(alloca);
                                } else if let Some(map_drop) = map_elem_drop {
                                    self.track_vec_of_maps_var(alloca, map_drop);
                                } else if let (Some(agg_drop), Some(et)) = (agg_elem_drop, elem_ty)
                                {
                                    self.track_vec_of_aggs_var(alloca, et, agg_drop);
                                } else {
                                    self.track_vec_var(alloca, elem_ty);
                                }
                            }
                            // Moved-in ownership (Map / File / enum /
                            // struct / user-Drop / SoA slots): the
                            // branch removed its cleanup action when it
                            // published the value (pre-fix it ran the
                            // action, freeing the handle/payload the
                            // parent was about to use — the
                            // `Map.new()`-in-a-branch UAF). Re-register
                            // the equivalent action against the
                            // parent's alloca so the moved-in value is
                            // freed exactly once at parent scope exit —
                            // same unique-owner shape as the
                            // `track_vec_var` re-track above.
                            self.register_slot_ownership(
                                &slot.binding_name,
                                alloca,
                                &slot_ownership,
                            );
                        }
                    }
                }
                let max_idx = group.statement_indices.iter().copied().max().unwrap_or(i);
                i = max_idx + 1;
            } else if covered.contains(&i) {
                // Mid-group index already emitted as part of an earlier
                // group-start dispatch.
                i += 1;
            } else {
                // Auto-par reduction lowering (slice 3b / 3b.4, 2026-05-19).
                // When the slice-1 analyzer tagged this top-level stmt as
                // a reduction and the loop matches the v1 supported shape
                // (`for k in 0..hi { ... }` or `while k < hi { ...; k = k
                // + 1; }` with i64 accumulator and `+` op), lower to a
                // `karac_par_reduce` call. Pass the parent body so the
                // while-shape path can peek `body.stmts[i - 1]` for the
                // loop var's `let mut k: T = 0;` init. `Some(())` means
                // lowered; `None` means the analyzer tagged it but the
                // codegen v1 doesn't yet handle that op/type/shape — fall
                // through to sequential.
                let lowered = self.try_emit_reduction_lowering(body, i)?;
                if lowered.is_none() {
                    self.compile_stmt(&body.stmts[i])?;
                }
                i += 1;
            }
            if self
                .builder
                .get_insert_block()
                .unwrap()
                .get_terminator()
                .is_some()
            {
                return Ok(None);
            }
        }

        if let Some(ref expr) = body.final_expr {
            let val = self.compile_tail_final_expr(expr, auto_par_tail)?;
            Ok(Some(val))
        } else {
            Ok(None)
        }
    }

    /// True iff `stmt` is a top-level `par { ... }` expression statement.
    /// Used in `compile_function_body` to skip auto-par groups that would
    /// otherwise re-parallelize an already-parallel block.
    pub(super) fn stmt_is_par_block(stmt: &Stmt) -> bool {
        matches!(&stmt.kind, StmtKind::Expr(e) if matches!(&e.kind, ExprKind::Par(_)))
    }

    /// Compute the per-group set of class-(ii) bindings — let-bindings
    /// defined inside the group's branches and read by stmts outside the
    /// group (or by `body.final_expr`). Slice A (Phase-7 — Par codegen:
    /// return values) replaces slice 2's drop-the-group gate with this
    /// function: each returned slot becomes a field in the synthesized
    /// `__karac_ParGroup_<id>_Returns` struct, the matching branch fn
    /// writes the alloca's value into the slot, and the parent reads it
    /// back after `karac_par_run` joins.
    ///
    /// The slot's `branch_index` is the position-within-group of the
    /// stmt (sorted by `statement_indices`), matching the index passed
    /// to `emit_par_branch_fn` so the slot-write emitter can dispatch
    /// per branch. Empty-result groups (the parallax-lite shape — three
    /// `writes(R_i)` with no binding read outside) return an empty Vec;
    /// `emit_par_run` then takes the same path with no slot machinery
    /// and emits byte-equivalent IR to slice 2.
    ///
    /// Bindings whose LLVM type can't be inferred (no annotation, no
    /// resolvable callee return type) are conservatively dropped from
    /// the slot list — those let-bindings will not be visible outside
    /// the group, but the rest of the group still parallelizes. In
    /// practice this only fires for closure / dynamic-dispatch RHSes
    /// that don't appear in the auto-par-eligible set.
    /// Compute return slots, returning `None` when some binding read
    /// outside the group has an RHS shape `infer_let_binding_llvm_type`
    /// can't recover the LLVM type from. In that case the caller
    /// should drop the par-group entirely and fall back to sequential
    /// compilation — emitting it with the binding silently absent from
    /// the slot list left the binding as a class-(i) branch-local
    /// alloca with no parent-scope propagation, producing
    /// "Undefined variable" errors at every later read site
    /// (the LeetCode 3629 kata's `compile_slice_index` panic family).
    pub(super) fn compute_return_slots_checked(
        &self,
        group: &ParallelGroup,
        body: &Block,
    ) -> Option<Vec<ReturnSlot<'ctx>>> {
        // 1. Collect names defined by stmts in this group, mapped to
        //    their branch_index (position in statement_indices when
        //    sorted). The branch fn order in `emit_par_run` follows the
        //    same sort: `group_stmts` is built by iterating
        //    `statement_indices` in their stored order. We sort here to
        //    keep slot layout deterministic regardless of analyzer
        //    iteration order.
        let mut sorted_indices = group.statement_indices.clone();
        sorted_indices.sort_unstable();
        let in_group: HashSet<usize> = sorted_indices.iter().copied().collect();

        // Auto-par + heap-env closures (B-2026-06-22-2, Vec-store slice): a
        // heap-env closure's fat pointer + reference-counted env box does NOT
        // survive the par-group return-struct round-trip — the env pointer is
        // mis-transferred across the `karac_par_run` join, so the joined binding
        // reads a closure with a dangling/garbage env (e.g. a `let f = make(k)`
        // grouped with an independent `let v = Vec.new()`). Bail any group that
        // constructs a heap-env closure to sequential codegen. Sound — sequential
        // is always correct, and a `make(..)`-shaped let is cheap, so the lost
        // parallelism is negligible. Mirrors the destructure-bind / captured-
        // mutation bail-outs below; the slot-type guard in step 3 additionally
        // catches a closure COPY whose RHS isn't a call.
        for &stmt_idx in &sorted_indices {
            if stmt_idx >= body.stmts.len() {
                continue;
            }
            if let StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } =
                &body.stmts[stmt_idx].kind
            {
                if self.is_heap_env_producing_call(value) {
                    return None;
                }
            }
        }

        // Per-binding metadata: which branch defines it, and what's the
        // statement reference for type inference.
        let mut defined: HashMap<String, (usize, &Stmt)> = HashMap::new();
        // Names bound by a NON-`Binding` let pattern in this group — tuple /
        // struct / slice destructure (`let (a, b) = pair()`). The return-slot
        // mechanism below is built around one-name-one-type-per-stmt
        // (`infer_let_binding_llvm_type` infers a single type per `let`), which
        // a multi-binding destructure breaks. So these names get NO return slot;
        // if any escapes the group it would be lifted into a branch fn and left
        // undefined in the parent body ("Undefined variable 'a'",
        // B-2026-06-13-6). Collect them and bail to sequential below if any is
        // read outside the group — correctness over a marginal parallelization
        // (slotting destructure bindings across the join is a future slice).
        let mut destructure_bound: HashSet<String> = HashSet::new();
        for (branch_idx, &stmt_idx) in sorted_indices.iter().enumerate() {
            if stmt_idx >= body.stmts.len() {
                continue;
            }
            let stmt = &body.stmts[stmt_idx];
            match &stmt.kind {
                StmtKind::Let { pattern, .. } | StmtKind::LetElse { pattern, .. } => {
                    if let PatternKind::Binding(name) = &pattern.kind {
                        defined.insert(name.clone(), (branch_idx, stmt));
                    } else {
                        for name in pattern.binding_names() {
                            destructure_bound.insert(name);
                        }
                    }
                }
                StmtKind::LetUninit { .. } => {
                    // LetUninit has no immediate value; tracked only as a
                    // "name defined" — the slot value is whatever later
                    // assignment writes. Slice A doesn't lift this case
                    // (would require slot writes from arbitrary assigns).
                }
                _ => {}
            }
        }
        // 2. Walk every stmt outside the group + final_expr collecting
        //    reads. Names actually consumed outside become slots; names
        //    only used inside the group remain class-(i) — branch-local
        //    allocas with no slot. Computed before the captured-mutation
        //    check AND before the `defined.is_empty()` early return —
        //    both consume `refs`, and the captured-mutation check must
        //    fire even for side-effect-only groups (which have no let
        //    bindings, so `defined` is empty by construction).
        let mut refs: HashSet<String> = HashSet::new();
        let mut defs: HashSet<String> = HashSet::new();
        for (idx, stmt) in body.stmts.iter().enumerate() {
            if in_group.contains(&idx) {
                continue;
            }
            match &stmt.kind {
                StmtKind::Let { pattern, value, .. } | StmtKind::LetElse { pattern, value, .. } => {
                    self.refs_in_expr(value, &mut refs, &mut defs);
                    for name in pattern.binding_names() {
                        defs.insert(name);
                    }
                }
                StmtKind::Expr(e) => self.refs_in_expr(e, &mut refs, &mut defs),
                StmtKind::Assign { target, value } => {
                    self.refs_in_expr(target, &mut refs, &mut defs);
                    self.refs_in_expr(value, &mut refs, &mut defs);
                }
                StmtKind::CompoundAssign { target, value, .. } => {
                    self.refs_in_expr(target, &mut refs, &mut defs);
                    self.refs_in_expr(value, &mut refs, &mut defs);
                }
                _ => {}
            }
        }
        if let Some(e) = &body.final_expr {
            self.refs_in_expr(e, &mut refs, &mut defs);
        }

        // 2.5. Reject groups whose branches would silently lose
        //      captured-local mutations. Auto-par captures each local
        //      bit-for-bit into the per-branch env struct (see
        //      `emit_par_run` step 3), so a branch that mutates a
        //      captured `Vec`/`Map`/scalar via `v.push(...)` /
        //      `cap = max` / etc. mutates only its own local copy —
        //      the parent's view is the pre-spawn snapshot. The
        //      return-slot mechanism propagates *let-introduced*
        //      bindings back across the join, but a mutation that
        //      doesn't introduce a new name has no slot, so it's
        //      silently dropped. If any such mutation targets a name
        //      read outside the group, fall back to sequential
        //      compilation — the analyzer's parallelization is an
        //      optimization hint, not a semantic requirement, and
        //      sequential is correct here.
        //
        //      Detection lives in the analyzer (`StmtInfo.defines −
        //      StmtInfo.let_introduced`, unioned across group stmts)
        //      because that's where method-mutates-receiver is
        //      already decided via `method_effects_imply_receiver_mutation`.
        //      Doing it here would either duplicate that effects
        //      lookup or use a coarser "any method call mutates"
        //      heuristic that would over-serialize pure-method
        //      patterns like `let s = data.as_slice();`.
        //
        //      Runs *before* the `defined.is_empty()` early return so
        //      side-effect-only groups (e.g. `a.bump_a(); b.bump_b()`
        //      with no `let` bindings) are still gated — without this
        //      ordering, the early return would emit a par-run that
        //      silently drops the mutations.
        if !group.captured_mutations.is_disjoint(&refs) {
            return None;
        }

        // 2.6. Reject groups that destructure-bind a name read outside the
        //      group. A tuple/struct/slice `let` pattern produces several names
        //      of differing types from one statement, which the single-type-
        //      per-`let` return-slot path (step 3) can't materialize — so such
        //      a binding would be lifted into a branch fn with no slot and left
        //      undefined in the parent body (B-2026-06-13-6). Fall back to
        //      sequential, exactly as the captured-mutation check above does.
        if !destructure_bound.is_disjoint(&refs) {
            return None;
        }

        // No let-introduced bindings to materialize as slots — the
        // group is side-effect-only and the captured-mutation check
        // above already cleared it. Empty-slot par-run is correct.
        if defined.is_empty() {
            return Some(Vec::new());
        }

        // 3. For each defined name read outside, infer the LLVM type.
        //    Sort by binding_name within each branch for deterministic
        //    slot layout.
        let mut slots: Vec<ReturnSlot<'ctx>> = Vec::new();
        let mut names_with_branch: Vec<(usize, String, &Stmt)> = defined
            .into_iter()
            .filter(|(name, _)| refs.contains(name))
            .map(|(name, (branch_idx, stmt))| (branch_idx, name, stmt))
            .collect();
        names_with_branch.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        for (branch_idx, name, stmt) in names_with_branch {
            {
                let llvm_ty = self.infer_let_binding_llvm_type(stmt)?;
                // A closure-valued return slot (a heap-env closure binding, or a
                // COPY of one) can't round-trip the par-group join — see the
                // group-level bail above. The construction-site `is_heap_env_producing_call`
                // check catches a fresh `make(..)`; this catches a `let g = f`
                // copy whose inferred type is the closure fat pointer. Bail to
                // sequential (B-2026-06-22-2, Vec-store slice). Gated to programs
                // that actually define an escaping closure so a closure-free
                // program with a coincidental `{ptr, ptr}`-typed slot (e.g. a
                // two-pointer tuple) is never de-parallelized.
                if !self.fns_returning_heap_env.is_empty()
                    && llvm_ty == self.closure_value_type().into()
                {
                    return None;
                }
                slots.push(ReturnSlot {
                    binding_name: name,
                    branch_index: branch_idx,
                    llvm_ty,
                    var_type_name: Self::let_binding_annotation_type_name(stmt),
                });
            }
        }
        Some(slots)
    }

    /// Surface type NAME from a let-statement's explicit annotation (the
    /// first path segment, e.g. `u8` / `i32` / `String`), if present. The
    /// auto-par / `par`-block return-slot materialization re-registers this
    /// into `var_type_names` so a narrow *unsigned* slot binding keeps its
    /// signedness for `expr_is_unsigned_int` (B-2026-07-03-21). Returns
    /// `None` for an un-annotated let or a non-`Path` annotation shape.
    pub(super) fn let_binding_annotation_type_name(stmt: &Stmt) -> Option<String> {
        let ty_ann = match &stmt.kind {
            StmtKind::Let { ty, .. } | StmtKind::LetElse { ty, .. } => ty.as_ref()?,
            _ => return None,
        };
        match &ty_ann.kind {
            TypeKind::Path(p) => p.segments.first().cloned(),
            _ => None,
        }
    }

    /// Infer the LLVM type produced by a let-statement's RHS. Used by
    /// `compute_return_slots` to size each return-struct field before
    /// the branch fn is emitted. Tries (in order): explicit type
    /// annotation on the let, declared return type of a free-function
    /// call. Returns `None` for shapes the slot mechanism doesn't
    /// support (closures, untyped lets without annotations, generic
    /// monomorphized bodies that haven't been declared yet) — the
    /// caller drops the binding from the slot list, leaving it as a
    /// branch-local class-(i) binding instead.
    pub(super) fn infer_let_binding_llvm_type(&self, stmt: &Stmt) -> Option<BasicTypeEnum<'ctx>> {
        let (pattern, ty_ann, value): (&Pattern, Option<&TypeExpr>, &Expr) = match &stmt.kind {
            StmtKind::Let {
                pattern, ty, value, ..
            }
            | StmtKind::LetElse {
                pattern, ty, value, ..
            } => (pattern, ty.as_ref(), value),
            _ => return None,
        };
        // SoA-laid-out binding: a `Vec[E]` local whose name is a `layout` block is
        // physically the 4-field SoA struct, so its par-block / auto-par return
        // slot (and the parent join-bind alloca) must be the SoA struct type, not
        // the AoS `{ptr,len,cap}` header the `Vec[E]` annotation lowers to. The
        // `suspends` browser render loop's `grid` is threaded through such a slot
        // (the pool-driver boundary); without this it round-trips as a 3-field
        // header and mismatches its SoA use sites (`substep`/`render_fb`). Checked
        // before the annotation so the SoA type wins over the AoS `Vec[E]` lower.
        if let PatternKind::Binding(name) = &pattern.kind {
            if let Some(soa) = self.soa_layouts.get(name) {
                return Some(
                    self.soa_vec_type(soa.num_groups, soa.cold_group.is_some())
                        .into(),
                );
            }
        }
        if let Some(te) = ty_ann {
            return Some(self.llvm_type_for_type_expr(te));
        }
        // No annotation: statically infer the LLVM type of the RHS
        // expression. `infer_expr_llvm_type` covers calls, in-scope
        // aliases, literals, and — the shapes B-2026-07-02-31 exposed —
        // arithmetic/comparison binaries, unary ops, and block-expr
        // RHS (`let y = { ...; tail }`). An empty local-binding scope is
        // passed at the top level; block-expr recursion extends it with
        // the block's own `let` bindings so the tail expression's
        // identifier reads resolve.
        self.infer_expr_llvm_type(value, &HashMap::new())
    }

    /// Statically infer the LLVM type an expression will evaluate to,
    /// WITHOUT compiling it. Used by `infer_let_binding_llvm_type` to
    /// size par-block / auto-par return slots before the branch bodies
    /// are emitted (the return-struct layout must be known up front).
    ///
    /// `locals` carries the types of block-local `let` bindings visible
    /// at `expr` (for the block-expr RHS case) — it augments
    /// `self.variables` so a tail expression like `z + 1` (where `z` is
    /// a binding introduced earlier in the same block) resolves. It is
    /// empty at the top-level call.
    ///
    /// Conservative by design: any shape it cannot classify returns
    /// `None`, and the caller drops the slot (the branch still runs; a
    /// join-expression read of a dropped name surfaces the standard
    /// "Undefined variable" diagnostic, matching the pre-existing
    /// fallback contract). It never guesses a wrong type.
    pub(super) fn infer_expr_llvm_type(
        &self,
        expr: &Expr,
        locals: &HashMap<String, BasicTypeEnum<'ctx>>,
    ) -> Option<BasicTypeEnum<'ctx>> {
        match &expr.kind {
            // Integer / bool literals carry their type directly. Sized
            // integer suffixes (`0i32`, `5u8`, …) map through
            // `const_int_for_suffix`'s sizing rules; the unsuffixed
            // default is `i64`. Same for floats.
            ExprKind::Integer(_, sfx) => Some(self.const_int_for_suffix(0, *sfx).get_type().into()),
            ExprKind::Float(_, sfx) => {
                Some(self.const_float_for_suffix(0.0, *sfx).get_type().into())
            }
            ExprKind::Bool(_) => Some(self.context.bool_type().into()),

            // Identifier: a block-local binding introduced earlier in
            // the same block-expr wins over an outer local of the same
            // name (lexical shadowing); otherwise an in-scope variable
            // (param or earlier outer local) — `let n = p`.
            ExprKind::Identifier(name) => locals
                .get(name)
                .copied()
                .or_else(|| self.variables.get(name).map(|slot| slot.ty)),

            // Free-function call — read the declared return type from the
            // LLVM function declaration the declare-pass already minted.
            ExprKind::Call { callee, .. } => {
                if let ExprKind::Identifier(name) = &callee.kind {
                    // Niche-ABI callee: the DECLARED LLVM return type is a
                    // nullable ptr, but the in-body value shape the branch
                    // fn stores into the slot is the conventional 4-i64
                    // Option struct (`compile_call` unpacks at the call
                    // boundary). Size the slot for the unpacked shape — a
                    // ptr-sized slot would silently truncate the 32-byte
                    // store.
                    if self.fn_niche_abi.get(name).is_some_and(|abi| abi.ret) {
                        return Some(self.enum_layouts["Option"].llvm_type.into());
                    }
                    if let Some(fn_val) = self.module.get_function(name) {
                        if let Some(ret) = fn_val.get_type().get_return_type() {
                            return Some(ret);
                        }
                    }
                }
                // Lowered operator dispatch: the `lower` pass rewrites
                // `a + b` / `a == b` / `-a` into `Call { callee:
                // Path([target_type, op_method]), args }` (see
                // `lowering.rs::rewrite_binary` / `rewrite_unary`). These
                // are the exact shapes B-2026-07-02-31 exposed — a par
                // branch `let x = base + 1` reaches codegen as
                // `i64.add(base, 1)`, not an `ExprKind::Binary`. Infer the
                // result type from the operator's fixed semantics:
                //   - comparison ops → bool
                //   - arithmetic / bitwise / shift / neg / not → the
                //     target type (segments[0]), mapped via
                //     `llvm_type_for_name`
                if let ExprKind::Path { segments, .. } = &callee.kind {
                    if segments.len() == 2 {
                        let (target, method) = (segments[0].as_str(), segments[1].as_str());
                        match method {
                            "eq" | "ne" | "lt" | "le" | "gt" | "ge" => {
                                return Some(self.context.bool_type().into());
                            }
                            "add" | "sub" | "mul" | "div" | "rem" | "bitand" | "bitor"
                            | "bitxor" | "shl" | "shr" | "neg" | "not" => {
                                return Some(self.llvm_type_for_name(target));
                            }
                            _ => {}
                        }
                    }
                }
                None
            }

            // Binary: arithmetic / bitwise / shift yield the operand
            // type (infer from either operand — prefer the left, fall
            // back to the right when the left is un-inferrable, e.g.
            // `1 + f()` vs `f() + 1`); comparison / logical yield bool.
            // Range is not a slot-eligible scalar — leave it None.
            ExprKind::Binary { op, left, right } => match op {
                BinOp::Eq
                | BinOp::NotEq
                | BinOp::Lt
                | BinOp::LtEq
                | BinOp::Gt
                | BinOp::GtEq
                | BinOp::And
                | BinOp::Or => Some(self.context.bool_type().into()),
                BinOp::Range | BinOp::RangeInclusive => None,
                _ => self
                    .infer_expr_llvm_type(left, locals)
                    .or_else(|| self.infer_expr_llvm_type(right, locals)),
            },

            // Unary: `-x` / `~x` keep the operand type; `not x` is bool.
            // `*x` (Deref) is conservatively un-inferrable here.
            ExprKind::Unary { op, operand } => match op {
                UnaryOp::Not => Some(self.context.bool_type().into()),
                UnaryOp::Neg | UnaryOp::BitNot => self.infer_expr_llvm_type(operand, locals),
                UnaryOp::Deref => None,
            },

            // Block expression: its value is the tail expression's value.
            // Walk the block's own `let` bindings first, extending a
            // fresh local scope, then infer the tail against it. Nested
            // shadowing is handled because inner `let`s overwrite the
            // name in `inner`.
            ExprKind::Block(block) => self.infer_block_tail_llvm_type(block, locals),

            // `if cond { a } else { b }` as an expression: both arms have
            // the same type, so infer from whichever arm is inferrable.
            ExprKind::If {
                then_block,
                else_branch,
                ..
            } => self
                .infer_block_tail_llvm_type(then_block, locals)
                .or_else(|| {
                    else_branch
                        .as_ref()
                        .and_then(|e| self.infer_expr_llvm_type(e, locals))
                }),

            // `match s { ... }` as an expression: all arms unify to the
            // same type, so infer from the first arm whose body is
            // inferrable. Arm bodies that reference pattern-introduced
            // bindings are conservatively skipped (their names aren't in
            // `locals`); a later arm with a literal/arithmetic body still
            // pins the slot type.
            ExprKind::Match { arms, .. } => arms
                .iter()
                .find_map(|arm| self.infer_expr_llvm_type(&arm.body, locals)),

            // `v[i]` — Vec / Slice element read. B-2026-07-02-38 (residual
            // of B-2026-07-02-31): a par branch `let x = v[0]` reads one
            // element of an OUTER collection, and its slot must be sized to
            // that element's LLVM type — exactly the value shape
            // `compile_index` produces. Codegen already tracks the element
            // type of every compiled Vec / Slice local (`vec_elem_types` /
            // `slice_elem_types`); an OUTER collection was compiled before
            // this `par` block, so the lookup resolves. A block-local
            // collection isn't registered yet at slot-sizing time and falls
            // through to `None` (the pre-existing branch-local contract).
            ExprKind::Index { object, .. } => {
                if let ExprKind::Identifier(base) = &object.kind {
                    if let Some(&elem) = self.vec_elem_types.get(base.as_str()) {
                        return Some(elem);
                    }
                    if let Some(&elem) = self.slice_elem_types.get(base.as_str()) {
                        return Some(elem);
                    }
                }
                None
            }

            // `o.field` — struct field read. B-2026-07-02-38: resolve the
            // receiver's declared type via `var_type_names` (struct-kind
            // locals / the synthesized `self` param), find the field's
            // declaration index, and map its `TypeExpr` to an LLVM type.
            // Only OUTER struct locals carry a `var_type_names` entry at
            // slot-sizing time; anything else stays un-inferrable.
            ExprKind::FieldAccess { object, field } => {
                let ty_name = self.inferred_receiver_type(object)?;
                let idx = self
                    .struct_field_names
                    .get(&ty_name)?
                    .iter()
                    .position(|f| f == field)?;
                let te = self.struct_field_type_exprs.get(&ty_name)?.get(idx)?;
                Some(self.llvm_type_for_type_expr(te))
            }

            // `r.method(args)` — method-call read. B-2026-07-02-38, two
            // resolvable shapes:
            //   1. User impl method: the impl pass emits `Type.method` as an
            //      LLVM function whose declared return type is authoritative
            //      (the declare-pass runs before body lowering, so the fn
            //      exists by the time slots are sized). Mirrors the
            //      `ExprKind::Call` free-function arm.
            //   2. Value-preserving numeric scalar builtin (`abs` / `sqrt` /
            //      `pow` / the `float_math` transcendentals): the typechecker
            //      types these as `x.m(..) -> Self` (see
            //      `expr_method_call.rs`), so the result is the receiver's own
            //      numeric type — which is exactly the receiver's INFERRED
            //      LLVM type, because codegen widens every integer local to
            //      i64 in both storage and value flow and keeps floats at
            //      their own width. That i64-backing is why a source-level
            //      narrow annotation (`let n: i32`) still resolves here: its
            //      slot IS i64. The i64/float guard is therefore not a
            //      narrowing exclusion but a receiver-shape filter — it
            //      rejects a non-numeric receiver (a struct, `bool` (i1),
            //      `char` (i32), …) whose `-> Self` claim these methods don't
            //      make, so the slot stays un-inferrable rather than mis-sized.
            ExprKind::MethodCall { object, method, .. } => {
                if let Some(ty_name) = self.inferred_receiver_type(object) {
                    if let Some(fn_val) = self.module.get_function(&format!("{ty_name}.{method}")) {
                        if let Some(ret) = fn_val.get_type().get_return_type() {
                            return Some(ret);
                        }
                    }
                }
                let returns_receiver_numeric = matches!(method.as_str(), "abs" | "sqrt")
                    || crate::float_math::classify(method).is_some();
                if returns_receiver_numeric {
                    match self.infer_expr_llvm_type(object, locals) {
                        Some(BasicTypeEnum::IntType(t)) if t.get_bit_width() == 64 => {
                            return Some(t.into());
                        }
                        Some(recv @ BasicTypeEnum::FloatType(_)) => return Some(recv),
                        _ => {}
                    }
                }
                None
            }

            // Anything else (closures, struct literals, tuple index, etc.):
            // conservatively un-inferrable here. The slot is dropped; the
            // pre-existing "un-inferrable RHS → branch-local" contract
            // applies.
            _ => None,
        }
    }

    /// Infer the LLVM type of a block's tail (`final_expr`), threading
    /// the block's own `let` bindings into a fresh local scope layered
    /// over `outer`. A block with no tail expression evaluates to unit
    /// and is not slot-eligible → `None`.
    fn infer_block_tail_llvm_type(
        &self,
        block: &Block,
        outer: &HashMap<String, BasicTypeEnum<'ctx>>,
    ) -> Option<BasicTypeEnum<'ctx>> {
        let mut inner = outer.clone();
        for stmt in &block.stmts {
            if let StmtKind::Let { pattern, value, .. } | StmtKind::LetElse { pattern, value, .. } =
                &stmt.kind
            {
                if let PatternKind::Binding(name) = &pattern.kind {
                    if let Some(t) = self.infer_expr_llvm_type(value, &inner) {
                        inner.insert(name.clone(), t);
                    } else {
                        // Un-inferrable inner binding: remove any stale
                        // outer entry so a later reference doesn't
                        // resolve to the wrong (shadowed) type.
                        inner.remove(name);
                    }
                }
            }
        }
        block
            .final_expr
            .as_ref()
            .and_then(|e| self.infer_expr_llvm_type(e, &inner))
    }

    /// Phase-B2 link-store fast path (see the `StmtKind::Assign` arm):
    /// `<bare cluster>.link = Some(<fresh>)` / `= None` lowers to one
    /// pointer store into the niche slot. Returns Ok(true) when the
    /// store was emitted; Ok(false) falls back to the generic path
    /// (which is count-correct for every shape, so the fallback is
    /// always safe).
    fn try_emit_b2_link_store(&mut self, target: &Expr, value: &Expr) -> Result<bool, String> {
        let ExprKind::FieldAccess { object, field } = &target.kind else {
            return Ok(false);
        };
        let ExprKind::Identifier(obj) = &object.kind else {
            return Ok(false);
        };
        let Some(b2) = self.b2_binding(obj).cloned() else {
            return Ok(false);
        };
        if matches!(b2.role, B2Role::OptionCursor) {
            return Ok(false);
        }
        // The stored field must be the cluster's link field, and the
        // link slot must be niche-shaped (single ptr).
        let link_name = self
            .struct_field_names
            .get(&b2.member_type)
            .and_then(|ns| ns.get(b2.link_field_index))
            .cloned();
        if link_name.as_deref() != Some(field.as_str()) {
            return Ok(false);
        }
        if self
            .niche_field_inner_heap_type(&b2.member_type, b2.link_field_index)
            .is_none()
        {
            return Ok(false);
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        // Value: Some(<fresh>) → the fresh binding's heap ptr; None →
        // null. Anything else falls back.
        let new_ptr = match &value.kind {
            ExprKind::Identifier(n) if n == "None" => ptr_ty.const_null(),
            ExprKind::Call { callee, args } if args.len() == 1 => {
                let ExprKind::Identifier(c) = &callee.kind else {
                    return Ok(false);
                };
                if c != "Some" {
                    return Ok(false);
                }
                let ExprKind::Identifier(v) = &args[0].value.kind else {
                    return Ok(false);
                };
                let is_fresh = self
                    .b2_binding(v)
                    .is_some_and(|b| matches!(b.role, B2Role::Fresh));
                if !is_fresh {
                    return Ok(false);
                }
                let Some(vslot) = self.variables.get(v.as_str()).copied() else {
                    return Ok(false);
                };
                self.builder
                    .build_load(ptr_ty, vslot.ptr, &format!("{v}.b2.link.val"))
                    .unwrap()
                    .into_pointer_value()
            }
            _ => return Ok(false),
        };
        let Some(oslot) = self.variables.get(obj.as_str()).copied() else {
            return Ok(false);
        };
        let heap_type = self
            .shared_types
            .get(&b2.member_type)
            .map(|i| i.heap_type)
            .expect("b2 member type registered in shared_types");
        // Phase-D layout: a headerless member's link slot sits at the
        // un-shifted user index in the twin type.
        let (gep_ty, base) = self.shared_gep_layout(&b2.member_type, heap_type);
        let obj_ptr = self
            .builder
            .build_load(ptr_ty, oslot.ptr, &format!("{obj}.b2.link.obj"))
            .unwrap()
            .into_pointer_value();
        let field_ptr = self
            .builder
            .build_struct_gep(
                gep_ty,
                obj_ptr,
                b2.link_field_index as u32 + base,
                "b2.link.slot",
            )
            .unwrap();
        self.builder.build_store(field_ptr, new_ptr).unwrap();
        Ok(true)
    }

    /// If `value` is a call whose result is a borrow (`-> ref T` /
    /// `-> mut ref T`), return the inner `T`'s `TypeExpr`. Used by the `let`
    /// arm to bind the result as a ref-local rather than a value (caller
    /// half of B-2026-06-07-5). Free-function calls resolve by name via
    /// `fn_ref_return_inner`; method calls (`u.name()`) have no static name
    /// to key, so they resolve by the call expression's span through the
    /// `ref_return_inner_types` table the lowering pass derived from the
    /// typechecker. Method-call chains (`s.split(' ').first()`) remain a
    /// tracked follow-on.
    /// True when `e` is a borrow-returning **free-function** call
    /// (`echo(t)` where `echo -> ref T`). Used to admit such a call in
    /// tail/return position of a `-> ref T` function (chained borrow
    /// returns, B-2026-06-07-5): the call lowers to a `ptr` (the borrow
    /// ABI), so the compiled value IS the borrow address and is returned
    /// directly — no `compile_ref_return_ptr` address re-derivation, which
    /// for a call would emit it twice. Method-call chains stay out of scope
    /// (kept in lockstep with `classify_borrow_return_call` on the ownership
    /// side, which also admits free-fn calls only).
    pub(super) fn is_borrow_returning_call_expr(&self, e: &Expr) -> bool {
        matches!(&e.kind, ExprKind::Call { callee, .. }
            if matches!(&callee.kind, ExprKind::Identifier(n)
                if self.fn_ref_return_inner.contains_key(n)))
    }

    fn ref_return_inner_for_call(&self, value: &Expr) -> Option<TypeExpr> {
        match &value.kind {
            ExprKind::Call { callee, .. } => {
                if let ExprKind::Identifier(name) = &callee.kind {
                    return self.fn_ref_return_inner.get(name).cloned();
                }
                None
            }
            // Only USER-defined ref accessors route through the method-ref
            // path; builtin ref-returning methods (`or_insert`, `get`, …)
            // keep their dedicated codegen.
            ExprKind::MethodCall { method, .. } if self.user_ref_method_names.contains(method) => {
                self.ref_return_inner_types
                    .get(&(value.span.offset, value.span.length))
                    .cloned()
            }
            _ => None,
        }
    }

    /// B-2026-06-10-2 fix. If `value` is `P.field` where `P` is an owned
    /// (bare, non-ref) by-value struct PARAM, deep-copy the Vec/String field
    /// buffer that was just bound into `var_name`'s slot, so the moved-out
    /// local owns an independent buffer.
    ///
    /// A by-value struct param is a shallow copy whose heap-field buffers alias
    /// the caller's; the caller retains and frees them (its scope-exit
    /// struct-drop). Moving a field out (`let inner = h.v`) binds a shallow
    /// alias that this function then tracks for a `FreeVecBuffer` — so without
    /// the copy, `free(inner.data)` here and the caller's
    /// `__karac_drop_struct_<T>` free the SAME buffer (double-free). The copy
    /// makes the two frees hit independent buffers. Gated to PARAM sources: a
    /// field moved out of a LOCAL struct this function owns is handled by the
    /// existing in-function cap-zero suppression (it can reach the local's
    /// slot; a cross-function caller's slot it cannot). Bare-`Vec`/`String`
    /// params already deep-copy at the call site (`owned_vecstr_params`); this
    /// is the one-level-in analogue.
    fn deep_copy_owned_struct_param_field_move(
        &mut self,
        var_name: &str,
        value: &Expr,
        elem_ty: BasicTypeEnum<'ctx>,
    ) {
        // Source is a field of a caller-retains by-value struct PARAM
        // (`owned_struct_params`) OR of a heap `for`-loop struct/enum ELEMENT
        // (`for_loop_owned_agg_vars`, B-2026-07-04-17) — both retain + free the
        // field buffer at their own teardown (the param's struct-drop / the
        // container's per-element drain), so a field moved into a fresh local
        // must own an independent copy.
        let is_param_field = matches!(
            &value.kind,
            ExprKind::FieldAccess { object, .. }
                if matches!(&object.kind, ExprKind::Identifier(p)
                    if self.owned_struct_params.contains(p.as_str())
                        || self.for_loop_owned_agg_vars.contains(p.as_str()))
        );
        // B-2026-07-09-12 clone-on-extract (field-access-move form) — the source is
        // a Vec field of a shared-enum-payload VIEW (`let a = c.args`). Same hazard
        // as the destructure form: the moved-out Vec aliases the box's buffer +
        // element handles, and the leaf's own drop plus the box's rc-drop both free
        // them. Deep-copy the buffer, and for a `Vec[shared]` also rc-INC each
        // element (the box keeps its originals). The bare-`shared` field-move is
        // already balanced by `compile_field_access`'s read-inc, so only the Vec
        // form needs this.
        let is_view_field = matches!(
            &value.kind,
            ExprKind::FieldAccess { object, .. }
                if matches!(&object.kind, ExprKind::Identifier(p)
                    if self.shared_enum_payload_view_vars.contains_key(p.as_str()))
        );
        if !is_param_field && !is_view_field {
            return;
        }
        let slot_ptr = match self.variables.get(var_name) {
            Some(s) => s.ptr,
            None => return,
        };
        let vec_ty = self.vec_struct_type();
        let elem_te = self.var_elem_type_exprs.get(var_name).cloned();
        if let Ok(cur) = self
            .builder
            .build_load(vec_ty, slot_ptr, "move.field.copy.cur")
        {
            let copied = self.emit_vecstr_defensive_copy(cur, elem_ty, elem_te.as_ref());
            let _ = self.builder.build_store(slot_ptr, copied);
        }
        if is_view_field {
            if let Some(heap_type) = elem_te
                .as_ref()
                .and_then(|te| self.shared_heap_type_for_type_expr(te))
            {
                self.rc_inc_vec_shared_elements(slot_ptr, heap_type);
            }
        }
    }

    /// B-2026-07-04-17: deep-copy the field(s) of a just-bound aggregate at
    /// `alloca` that alias a heap-owning `for`-loop struct ELEMENT
    /// (`for_loop_owned_agg_vars`), so the new binding's struct-drop and the
    /// container's per-element drain free independent heap. Two shapes:
    ///  - `let x = a` (whole-element move): copy EVERY heap field of `x`.
    ///  - `let w = A { .. f: a.g .. }`: copy ONLY the field(s) whose init reads
    ///    heap OUT of the element — a sibling field from a fresh value keeps its
    ///    sole owner (copying it would leak). Fields are matched by NAME to the
    ///    declared (physical) order so the in-place field GEP is correct.
    fn deep_copy_for_loop_agg_element_move(
        &mut self,
        value: &Expr,
        alloca: PointerValue<'ctx>,
        struct_name: &str,
    ) {
        match &value.kind {
            ExprKind::Identifier(src) if self.for_loop_owned_agg_vars.contains(src.as_str()) => {
                self.deep_copy_struct_heap_fields_in_place(alloca, struct_name);
            }
            ExprKind::StructLiteral { fields, .. } => {
                let Some(&st) = self.struct_types.get(struct_name) else {
                    return;
                };
                let Some(ftes) = self.struct_field_type_exprs.get(struct_name).cloned() else {
                    return;
                };
                let names = self.struct_field_names.get(struct_name).cloned();
                for field_init in fields {
                    let from_elem = match &field_init.value.kind {
                        ExprKind::FieldAccess { object, .. } => matches!(
                            &object.kind,
                            ExprKind::Identifier(a)
                                if self.for_loop_owned_agg_vars.contains(a.as_str())
                        ),
                        ExprKind::Identifier(a) => {
                            self.for_loop_owned_agg_vars.contains(a.as_str())
                        }
                        _ => false,
                    };
                    if !from_elem {
                        continue;
                    }
                    let idx = match names
                        .as_ref()
                        .and_then(|ns| ns.iter().position(|n| n == &field_init.name))
                    {
                        Some(i) => i,
                        None => continue,
                    };
                    if let Some(fte) = ftes.get(idx) {
                        self.deep_copy_one_aggregate_field(alloca, st, idx as u32, fte);
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) fn compile_stmt(&mut self, stmt: &Stmt) -> Result<(), String> {
        // Slice c-repl.B.5.1: REPL value-snapshot replay short-circuit.
        // When this stmt is a top-level `let <name> = <expr>` whose
        // binding name is in `snapshot_replay`, skip the original
        // RHS entirely and bind `<name>` to a load from the cell-
        // spanning `__karac_repl_snapshot_<name>` global instead. The
        // prior cell's codegen captured the value into that global,
        // so the binding sees the same value the user would have got
        // from re-evaluating the RHS — minus the RHS's side effects.
        // This closes the interpreter-vs-JIT semantic gap for
        // primitive-typed lets (see slice c-repl.B.5 design).
        if self.try_compile_snapshot_replay(stmt)? {
            return Ok(());
        }

        // Detect `let _ = m.insert(k, v)` / bare `m.insert(k, v);` where V
        // is a shared struct/enum. The flag is consumed by the `insert`
        // arm of `compile_map_method` to emit a follow-up rc_dec on the
        // displaced value — without it, every overwrite on a `Map[K,
        // sharedV]` leaks one ref (the `Some(old)` payload that the
        // discard never holds). Set unconditionally to false here so a
        // prior statement's stale flag never bleeds into this one.
        self.pending_map_insert_old_dec = false;
        if let Some((receiver_name, method)) = Self::stmt_discards_method_call(stmt) {
            if method == "insert" && self.map_val_shared_heap_type_for(receiver_name).is_some() {
                self.pending_map_insert_old_dec = true;
            }
        }

        // B-2026-06-17-2 — a discarded `spawn(...);` / `tg.spawn(...);` throws
        // away its `TaskHandle`, so no join will ever free the runtime handle.
        // Flag it (cleared unconditionally so a prior statement never bleeds
        // through) for `lower_spawn_shared` to mark detached → eager-reaped.
        self.pending_spawn_detach = Self::stmt_is_discarded_spawn(stmt);
        match &stmt.kind {
            // Slice 5 (general owned-temp tracking): `let _ = make();` /
            // `let _ = { make() };` discards a fresh owned temp with no
            // binding to drop it, so its heap buffer would leak. Route the
            // discarded tail through the owned-temp chokepoint inside a
            // one-shot frame so it drops at the `;`. Gated to a Wildcard
            // pattern whose RHS tail yields a fresh owned temp — every other
            // `let` shape (real bindings, f-string RHS, `let _ = <place>`)
            // falls through to the general arm unchanged. `pending_map_insert_
            // old_dec` for the `let _ = m.insert(k, v)` shape was already set
            // above the `match` and is consumed inside `compile_expr`; the
            // chokepoint no-ops on the returned `Option`, so the displaced-
            // value rc_dec path is untouched.
            StmtKind::Let { pattern, value, .. }
                if matches!(&pattern.kind, PatternKind::Wildcard)
                    && Self::discarded_owned_temp_tail(value).is_some() =>
            {
                let tail = Self::discarded_owned_temp_tail(value)
                    .expect("guard guarantees a discarded owned-temp tail");
                let tail_key = (tail.span.offset, tail.span.length);
                self.scope_cleanup_actions.push(Vec::new());
                let val = self.compile_expr(value)?;
                self.free_discarded_request_builder_temp(value, val);
                self.materialize_owned_temp(val, tail_key);
                self.drain_top_frame_with_emit();
                Ok(())
            }
            StmtKind::Let {
                pattern, value, ty, ..
            } => {
                // Borrow-returning call bound to a name (`let n = name_of(u)`
                // where `name_of -> ref T`): the RHS evaluates to a `ptr`
                // (the borrow's address), not a value. Bind it as a
                // ref-local — store the ptr, register it in `ref_params` so
                // every use derefs (symmetric to a `ref` parameter), and
                // queue NO heap cleanup (a borrow owns nothing; freeing it
                // would double-free the source). Caller half of
                // B-2026-06-07-5. Sits ahead of the value-oriented Vec/String
                // tracking below, which would mis-handle the raw pointer.
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    // Shared-ownership inc-on-copy (B-2026-06-22-2): `let g = f`
                    // where `f` is a heap-env closure binding. Both owners share
                    // the SAME RC env box, so copy the fat pointer, INCREMENT the
                    // env refcount, and register `g`'s own `FreeClosureEnv`
                    // cleanup — each owner RC-drops at scope exit, so the box is
                    // freed exactly once. Marking `g` in `heap_env_closure_vars`
                    // makes copies-of-copies (`let h = g`) work and keeps the
                    // misuse guard's owner-set reasoning consistent. Sits ahead
                    // of the generic fn-value path below, which would copy the
                    // pointer WITHOUT the inc (leaving the box under-counted →
                    // premature free / use-after-free).
                    if let ExprKind::Identifier(src) = &value.kind {
                        if self.heap_env_closure_vars.contains(src) {
                            let (src_ptr, src_ty) = {
                                let s = &self.variables[src];
                                (s.ptr, s.ty)
                            };
                            let fat = self
                                .builder
                                .build_load(src_ty, src_ptr, "clo.copy.fat")
                                .unwrap();
                            self.emit_heap_closure_env_inc(fat);
                            let fn_val = self.current_fn.expect("let inside a function");
                            let alloca = self.create_entry_alloca(fn_val, var_name, src_ty);
                            self.builder.build_store(alloca, fat).unwrap();
                            self.variables.insert(
                                var_name.clone(),
                                VarSlot {
                                    ptr: alloca,
                                    ty: src_ty,
                                },
                            );
                            if let Some(ft) = self.closure_fn_types.get(src).copied() {
                                self.closure_fn_types.insert(var_name.clone(), ft);
                            }
                            if let Some(frame) = self.scope_cleanup_actions.last_mut() {
                                frame.push(super::state::CleanupAction::FreeClosureEnv {
                                    fat_alloca: alloca,
                                });
                            }
                            self.heap_env_closure_vars.insert(var_name.clone());
                            return Ok(());
                        }
                    }
                    // First-class fn-value binding (B-2026-06-20-1 / -06-21-1 /
                    // -06-21-2). When this `let` binds a fn value — an explicit
                    // `Fn(...)` annotation, a bare free-fn name, or a call whose
                    // callee returns `Fn(...)` — store the closure fat pointer
                    // the RHS now produces (a bare fn name lowers to one via the
                    // free-fn-as-value source arm) and register the local in
                    // `closure_fn_types`, so both `apply(f, x)` (pass to a
                    // `Fn`-typed param) and `f(x)` (direct call through the
                    // local) work. Without this the binding held a raw `ptr`:
                    // passing it to a fat-pointer param failed LLVM verification,
                    // and a direct call fell through to the unknown-callee path
                    // (silently returned 0). The fat pointer's env is null and
                    // the trampoline is a module global, so the binding owns no
                    // heap — no scope cleanup, hence the early return. Sits ahead
                    // of the value-oriented tracking below (a fat pointer would
                    // mislead it). `let_binding_fn_value_type` returns `None`
                    // (falls through to the normal path) unless this really is a
                    // fn-value binding whose signature is recoverable here.
                    if let Some(fn_type) = self.let_binding_fn_value_type(ty.as_ref(), value) {
                        let fat = self.compile_expr(value)?;
                        let fn_val = self.current_fn.expect("let inside a function");
                        let alloca = self.create_entry_alloca(fn_val, var_name, fat.get_type());
                        self.builder.build_store(alloca, fat).unwrap();
                        self.variables.insert(
                            var_name.clone(),
                            VarSlot {
                                ptr: alloca,
                                ty: fat.get_type(),
                            },
                        );
                        self.closure_fn_types.insert(var_name.clone(), fn_type);
                        // Slice 1 (B-2026-06-22-2): if the RHS is a call to a
                        // function that returns a heap-env closure, this binding
                        // now OWNS that reference-counted env. Register the
                        // scope-exit RC-drop and mark it so a not-yet-supported
                        // escape (return / copy / store of the binding) is
                        // rejected rather than freed twice / leaked.
                        let rhs_is_heap_env_call = matches!(&value.kind,
                            ExprKind::Call { callee, .. }
                                if matches!(&callee.kind, ExprKind::Identifier(n)
                                    if self.fns_returning_heap_env.contains(n)));
                        if rhs_is_heap_env_call {
                            if let Some(frame) = self.scope_cleanup_actions.last_mut() {
                                frame.push(super::state::CleanupAction::FreeClosureEnv {
                                    fat_alloca: alloca,
                                });
                            }
                            self.heap_env_closure_vars.insert(var_name.clone());
                        }
                        return Ok(());
                    }
                    // `let r = m.entry(k).or_insert(d)` — bind `r` to the slot
                    // pointer (`mut ref V`) and tag it in `entry_slot_ref_vars`
                    // so `*r` reads / `*r += 1` / `*r = v` write through to the
                    // live map slot (the two-step counter idiom; codegen analog
                    // of the interpreter's `Value::MapSlotRef`). Sits ahead of
                    // the value-oriented tracking below, which would mis-handle
                    // the raw slot pointer.
                    if let Some(map_name) = self.entry_chain_or_insert_map_name(value) {
                        let val_ty = *self.map_val_types.get(&map_name).ok_or_else(|| {
                            format!("entry let-binding: missing val type for '{}'", map_name)
                        })?;
                        let fn_val = self.current_fn.expect("let inside a function");
                        let slot_ptr = self.compile_expr(value)?;
                        let ptr_ty = self.context.ptr_type(AddressSpace::default());
                        let alloca = self.create_entry_alloca(fn_val, var_name, ptr_ty.into());
                        self.builder.build_store(alloca, slot_ptr).unwrap();
                        self.variables.insert(
                            var_name.clone(),
                            VarSlot {
                                ptr: alloca,
                                ty: ptr_ty.into(),
                            },
                        );
                        self.entry_slot_ref_vars.insert(var_name.clone(), val_ty);
                        return Ok(());
                    }
                    if let Some(inner_te) = self.ref_return_inner_for_call(value) {
                        let fn_val = self.current_fn.expect("let inside a function");
                        // Mark this as the one sanctioned borrow-return call
                        // site so `compile_call` emits the borrow pointer
                        // rather than rejecting it as an unsupported direct use.
                        self.compiling_ref_return_let_rhs = true;
                        let ptr_res = self.compile_expr(value);
                        self.compiling_ref_return_let_rhs = false;
                        let ptr_val = ptr_res?;
                        let ptr_ty = self.context.ptr_type(AddressSpace::default());
                        let alloca = self.create_entry_alloca(fn_val, var_name, ptr_ty.into());
                        self.builder.build_store(alloca, ptr_val).unwrap();
                        self.variables.insert(
                            var_name.clone(),
                            VarSlot {
                                ptr: alloca,
                                ty: ptr_ty.into(),
                            },
                        );
                        // A `ref Tensor` accessor (`let r = h.view()` where
                        // `view -> ref Tensor`): the method returns the block
                        // pointer BY VALUE (tensors use the by-value ref ABI,
                        // `ref_return_is_value_abi`), so `ptr_val` already IS
                        // the tensor value. Bind `r` as a BORROWED tensor var
                        // — indexable / shape / transform-able like an owned
                        // tensor, but with NO `FreeTensor` (the owner frees
                        // the block; a second free would double-free). This is
                        // the method analogue of the borrowed-tensor binding
                        // the normal let path takes for free-fn `-> ref Tensor`
                        // returns; without it, `r` would be a deref-on-use
                        // ref-local and `r[i, j]` (a tuple index) would not
                        // dispatch as a tensor.
                        if let Some(info) = self.tensor_var_info_from_type_expr(&inner_te) {
                            self.tensor_var_infos.insert(var_name.clone(), info);
                            return Ok(());
                        }
                        let inner_llvm = self.llvm_type_for_type_expr(&inner_te);
                        self.ref_params.insert(var_name.clone(), inner_llvm);
                        // Make use-site dispatch (field access, method calls,
                        // print formatting) see the borrowed value's type.
                        if let TypeKind::Path(p) = &inner_te.kind {
                            if let Some(seg) = p.segments.first() {
                                self.var_type_names.insert(var_name.clone(), seg.clone());
                            }
                        }
                        // Register the borrowed Vec/String element type so the
                        // value-receiver method dispatch (`compile_vec_method`,
                        // gated on `vec_elem_types`) fires for read-only methods
                        // beyond `len`/`is_empty` — `n.get(i)`, `n.contains(x)`,
                        // `n.first()`, `n.chars()`, `n.starts_with(p)`, …
                        // (B-2026-06-07-5). `get_data_ptr` already derefs the
                        // borrow ptr to the borrowed `{ptr,len,cap}`, so those
                        // arms read through the borrow correctly. This queues NO
                        // `FreeVecBuffer` — `vec_elem_types` is a type registry,
                        // not a drop list (the borrow-local arm returns before
                        // `track_vec_var`), so the source's buffer is never
                        // double-freed. Mutating methods can't reach here: `ref
                        // T` is an immutable borrow, so the typechecker rejects
                        // them upstream.
                        if let Some(elem_ty) = self.extract_vec_elem_type(&inner_te) {
                            self.vec_elem_types.insert(var_name.clone(), elem_ty);
                            if let Some(inner) = vec_inner_type_expr(&inner_te) {
                                self.var_elem_type_exprs.insert(var_name.clone(), inner);
                            }
                        } else if self.is_string_type_expr(&inner_te) {
                            self.vec_elem_types
                                .insert(var_name.clone(), self.context.i8_type().into());
                        }
                        if self.is_string_type_expr(&inner_te) {
                            self.string_vars.insert(var_name.clone());
                        }
                        return Ok(());
                    }
                }
                // Type-changing shadow dance (step 1 of 3 — clean slate).
                // If this `let` re-binds a single name already in scope, lift
                // the old binding's per-variable sidecar metadata out of the
                // maps so the registration block below writes the NEW binding's
                // class tags onto a clean slate — identical to a fresh binding
                // of the same type. This also fixes latent rebind false-gates
                // (e.g. the `map_key_types.contains_key(var_name)` guard on the
                // `Map.new()` fast path below would otherwise fire for a
                // non-map rebind of a name that used to hold a map). The lifted
                // metadata is reinstated in step 2 for the value-compile, then
                // discarded in step 3. The borrow-return shadow path above has
                // already returned, so this only runs for value-bound lets.
                let shadow_name: Option<String> = match &pattern.kind {
                    PatternKind::Binding(n) if self.variables.contains_key(n) => Some(n.clone()),
                    _ => None,
                };
                let mut shadow_old_meta = shadow_name.as_ref().map(|n| self.take_var_metadata(n));
                // Record the binding's instantiated generic-enum type
                // (`Option[String]`, `Result[_, String]`) keyed by *variable
                // name* so heap-payload enum `==` (`compile_enum_eq`) can
                // resolve the type argument at a use site without span-keyed
                // lookup — which collides across f-string interpolations (each
                // interp expr is re-parsed under a fixed-length
                // `fn __interp__() { … }` wrapper, so same-position operands in
                // different f-strings share a span). Prefer the annotation;
                // else the RHS's own (absolute, reliable) span entry.
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    let inst = ty
                        .as_ref()
                        .filter(|te| self.is_generic_named_enum_type_expr(te))
                        .cloned()
                        .or_else(|| self.enum_inst_type_from_span(value));
                    if let Some(inst) = inst {
                        self.enum_inst_var_types.insert(var_name.clone(), inst);
                    }
                }
                // Track Vec/String element types from type annotation or RHS.
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    let mut detected = false;
                    // A `let` of this name creates a fresh owned/typed binding,
                    // never a `for`-loop element borrow — clear any stale
                    // `for_loop_borrow_vars` membership left by an earlier loop
                    // var of the same name, else this binding would be wrongly
                    // defensive-copied at consume sites (leak via the
                    // source-suppress that pairs with the copy).
                    self.for_loop_borrow_vars.remove(var_name);
                    self.for_loop_owned_agg_vars.remove(var_name);
                    // `let it = s.chars()` — codegen materializes the
                    // char-iterator as an eager `Vec[char]` snapshot (see the
                    // `chars()` intercept in `compile_method_call`), so register
                    // the binding as `Vec[char]`: `vec_elem_types` (the i32
                    // codepoint element) drives method / for-loop dispatch, and
                    // `var_elem_type_exprs` (the `char` element type) makes
                    // `it.collect()`'s clone build a `Vec[char]` rather than a
                    // `String` — `receiver_collection_type_expr` distinguishes
                    // the two on exactly that map. B-2026-06-18-5.
                    let rhs_is_chars = matches!(
                        &value.kind,
                        ExprKind::MethodCall { method, args, .. }
                            if method == "chars" && args.is_empty()
                    );
                    if rhs_is_chars {
                        self.vec_elem_types
                            .insert(var_name.clone(), self.context.i32_type().into());
                        self.var_elem_type_exprs.insert(
                            var_name.clone(),
                            TypeExpr {
                                kind: TypeKind::Path(PathExpr {
                                    segments: vec!["char".to_string()],
                                    generic_args: None,
                                    span: value.span.clone(),
                                }),
                                span: value.span.clone(),
                            },
                        );
                        detected = true;
                    }
                    // Explicit type annotation: let v: Vec[T] = ... or let s: String = ...
                    if let Some(ref te) = ty {
                        // `let t: Tensor[T, [dims]] = ...` — register the
                        // binding's element type + static dims
                        // (`src/codegen/tensor.rs`); the pending-info
                        // threading below makes them visible to the
                        // `Tensor.zeros/ones/full` constructor arms.
                        if let Some(info) = self.tensor_var_info_from_type_expr(te) {
                            self.tensor_var_infos.insert(var_name.clone(), info);
                            detected = true;
                        }
                        // `let c: Column[T] = ...` — register the binding's
                        // element type (`src/codegen/column.rs`); the
                        // pending-info threading below makes it visible to
                        // the `Column.new/with_capacity/from_vec` arms.
                        if let Some(info) = self.column_var_info_from_type_expr(te) {
                            self.column_var_infos.insert(var_name.clone(), info);
                            detected = true;
                        }
                        if let Some(elem_ty) = self.extract_vec_elem_type(te) {
                            self.vec_elem_types.insert(var_name.clone(), elem_ty);
                            if let Some(inner) = vec_inner_type_expr(te) {
                                self.var_elem_type_exprs.insert(var_name.clone(), inner);
                            }
                            detected = true;
                        }
                        if self.is_string_type_expr(te) {
                            self.vec_elem_types
                                .insert(var_name.clone(), self.context.i8_type().into());
                            self.string_vars.insert(var_name.clone());
                            detected = true;
                        }
                        if let Some(elem_ty) = self.extract_slice_elem_type(te) {
                            self.slice_elem_types.insert(var_name.clone(), elem_ty);
                            if let Some(inner) = slice_inner_type_expr(te) {
                                self.var_elem_type_exprs.insert(var_name.clone(), inner);
                            }
                            detected = true;
                        }
                        // `let s: ref CStr = c"..."` — register the cstr
                        // binding so downstream registration heuristics
                        // (`as_bytes` slice inference) see it. Method
                        // dispatch itself keys off the typechecker-recorded
                        // `CStr.<method>`, not this set.
                        if Self::is_cstr_type_expr(te) {
                            self.cstr_vars.insert(var_name.clone());
                            detected = true;
                        }
                        if let Some((k_ty, v_ty)) = self.extract_map_kv_types(te) {
                            self.map_key_types.insert(var_name.clone(), k_ty);
                            self.map_val_types.insert(var_name.clone(), v_ty);
                            if let Some(k_name) = Self::extract_map_key_name(te) {
                                self.map_key_type_names.insert(var_name.clone(), k_name);
                            }
                            if let Some((k_te, v_te)) = map_kv_type_exprs(te) {
                                self.map_key_type_exprs.insert(var_name.clone(), k_te);
                                self.var_elem_type_exprs.insert(var_name.clone(), v_te);
                            }
                            detected = true;
                        }
                        if let Some(elem_ty) = self.extract_set_elem_type(te) {
                            self.set_elem_types.insert(var_name.clone(), elem_ty);
                            if let Some(elem_name) = Self::extract_set_elem_name(te) {
                                self.set_elem_type_names.insert(var_name.clone(), elem_name);
                            }
                            if let Some(elem_te) = set_inner_type_expr(te) {
                                self.set_elem_type_exprs.insert(var_name.clone(), elem_te);
                            }
                            detected = true;
                        }
                        // `SortedSet[T]` / `SortedMap[K,V]` register above under
                        // the `Set`/`Map` side-tables (shared `KaracMap`
                        // storage); mark them so iteration + min/max observe
                        // ascending order.
                        if crate::codegen::helpers::is_sorted_collection_type(te) {
                            self.sorted_collection_vars.insert(var_name.clone());
                        }
                    }
                    // Fall back on the typechecker-recorded surface type for
                    // the binding when no explicit annotation was written.
                    // `let mut q = VecDeque.new(); q.push_back(x);` infers
                    // `q: VecDeque[T]` from the downstream push call; the
                    // typechecker writes both `pattern_binding_types`
                    // ("Vec"/"VecDeque") and `pattern_binding_inner_types`
                    // (the inner `T`) at the binding pattern's span. Codegen
                    // needs these for method dispatch to find `q` in
                    // `vec_elem_types`. Symmetric to the explicit-annotation
                    // path above.
                    if !detected {
                        let key = (pattern.span.offset, pattern.span.length);
                        if let Some(surface) = self.pattern_binding_types.get(&key).cloned() {
                            // A refinement binding (`let n = "x" as Name` where
                            // `type Name = String where …`) dispatches as its
                            // base: normalize so the String/Vec branches below
                            // register `string_vars` / `vec_elem_types` and
                            // method dispatch sees the base (phase-9 step 5a).
                            let surface = self.refinement_base_name(&surface);
                            if surface == "Vec" || surface == "VecDeque" {
                                if let Some(elem_te) =
                                    self.pattern_binding_inner_types.get(&key).cloned()
                                {
                                    let elem_ty = self.llvm_type_for_type_expr(&elem_te);
                                    self.vec_elem_types.insert(var_name.clone(), elem_ty);
                                    self.var_elem_type_exprs.insert(var_name.clone(), elem_te);
                                    detected = true;
                                }
                            } else if surface == "String" || surface == "StringSlice" {
                                // Inferred-String bindings (`let r = lcp(strs);`,
                                // `let r = strs[0];` where the element is String)
                                // must register the same i8-elem Vec surface +
                                // `string_vars` membership that the explicit-
                                // annotation path (`let r: String = …`) and the
                                // RHS-shape heuristics (`let r = "lit"`) set —
                                // otherwise `r.len()` / `r.push(…)` dispatch
                                // falls through in `compile_method_call`. The
                                // typechecker records "String" in
                                // `pattern_binding_types` for `Type::Str`
                                // bindings via `bind_pattern_types`; without
                                // wiring it here, only annotated String bindings
                                // got the dispatch maps.
                                //
                                // `StringSlice` (`let w = s.slice(0, n)`) shares
                                // String's `{ptr,len,cap}` layout with `cap == 0`,
                                // so its read-methods dispatch identically; the
                                // borrow (`cap == 0`) means any scope-exit free is
                                // `cap > 0`-guarded to a no-op (design.md §
                                // StringSlice).
                                self.vec_elem_types
                                    .insert(var_name.clone(), self.context.i8_type().into());
                                self.string_vars.insert(var_name.clone());
                                detected = true;
                            } else if surface == "Map" || surface == "Set" {
                                // #28 (B-2026-06-14-9) — a Map/Set bound to a
                                // LOCAL from a PLACE source (`let mm = s.m` /
                                // `let mm = h.m.0`) with no annotation. The
                                // explicit-annotation path (`extract_map_kv_types`)
                                // is skipped, and this fallback otherwise registers
                                // only `var_type_names` — never the Map/Set dispatch
                                // side-tables (`map_key_types` / `map_val_types` /
                                // `set_elem_types`) — so `mm.len()` / `mm.get(k)`
                                // fell through method dispatch. Register them from
                                // the typechecker's recorded collection `TypeExpr`
                                // (`pattern_binding_inner_types`, the full
                                // `Map[K,V]` / `Set[T]`). DISPATCH-only:
                                // `register_var_from_type_expr` queues NO
                                // `FreeMapHandle`, and the let path's `track_map_var`
                                // is gated on a fresh-handle RHS (clone/union/…) —
                                // which a place source is not — so `mm` stays a
                                // caller-retains alias and the source/owner is the
                                // sole freer (no double-free).
                                if let Some(coll_te) =
                                    self.pattern_binding_inner_types.get(&key).cloned()
                                {
                                    self.register_var_from_type_expr(var_name, &coll_te);
                                    detected = true;
                                }
                            } else if surface == "Option" || surface == "Result" {
                                // B-2026-07-08-9: capture the concrete payload
                                // TypeExpr(s) for an Option/Result let binding
                                // (annotated OR inferred) so the f-string /
                                // println Display path can render Some(<T>)/None
                                // (Ok/Err). The typechecker records the full
                                // `Option[T]` / `Result[T,E]` at the binding
                                // span; routing it through
                                // `register_var_from_type_expr` hits the
                                // Option/Result arm there which fills
                                // `var_option_payload_te` / `var_result_payload_te`.
                                if let Some(full_te) =
                                    self.pattern_binding_inner_types.get(&key).cloned()
                                {
                                    self.register_var_from_type_expr(var_name, &full_te);
                                    detected = true;
                                }
                            }
                            // Mirror bind_pattern_values's `var_type_names`
                            // write so let-bound shared-struct handles
                            // (`let cur = nodes[0]; cur.left...`) reach
                            // `shared_type_for_expr` for downstream
                            // field-access / method-call dispatch. Without
                            // this, the field load on a let-bound shared
                            // handle falls through to the i64-zero default.
                            self.record_var_type_name(var_name.clone(), surface);
                        }
                    }
                    // Infer String from RHS: let s = "hello", let s = String::new(),
                    // or let s = a + b (string concat)
                    if !detected
                        && (matches!(&value.kind, ExprKind::StringLit(_))
                            || self.is_string_new_call(value)
                            || self.is_string_binary_op(value))
                    {
                        self.vec_elem_types
                            .insert(var_name.clone(), self.context.i8_type().into());
                        self.string_vars.insert(var_name.clone());
                    }
                    // Infer `ref CStr` from a `let s = c"..."` RHS — the
                    // unannotated mirror of the `is_cstr_type_expr` arm
                    // above (same split as StringLit ↔ `: String`).
                    if !detected && matches!(&value.kind, ExprKind::CStringLit { .. }) {
                        self.cstr_vars.insert(var_name.clone());
                    }
                    // Atomic[T] inferred from `let a = Atomic.new(v)` —
                    // the slot stores `v`'s primitive directly (see the
                    // Atomic arm in `llvm_type_for_type_expr`); we only
                    // need `var_type_names[a] = "Atomic"` here so
                    // `a.load(ord)` / `a.store(v, ord)` route through
                    // the atomic-memory-op arm in `compile_method_call`
                    // instead of the user-impl-block lookup (which
                    // would fail — `Atomic.load` / `.store` are
                    // compiler-builtins, not user methods). Covers both
                    // inferred (`let a = Atomic.new(0)`) and
                    // annotated (`let a: Atomic[i64] = Atomic.new(0)`)
                    // forms since the canonical constructor shape is
                    // the same; `pattern_binding_types` doesn't carry
                    // "Atomic" because the baked `struct Atomic[T]` is
                    // not fed into the typechecker's struct registry.
                    if self.is_atomic_new_call(value) {
                        self.var_type_names
                            .insert(var_name.clone(), "Atomic".to_string());
                        // Atomic[bool] tracking — see `atomic_var_inner_is_bool`
                        // docstring on `Codegen` for the i1/i8 mismatch story.
                        // Two detection paths cover the canonical shapes the
                        // migrate tool + hand-written code produce:
                        //   (a) explicit `let a: Atomic[bool] = ...` annotation
                        //   (b) inferred `let a = Atomic.new(<bool literal>)`
                        // The bare-binding case `let a = Atomic.new(x)` where
                        // `x` is a bool variable falls through — typechecker
                        // doesn't expose that here. Users hitting that shape
                        // get a clear codegen error and the annotation form
                        // resolves it.
                        let annotation_is_bool = ty
                            .as_ref()
                            .map(super::types_lowering::is_atomic_bool_type_expr)
                            .unwrap_or(false);
                        let arg_is_bool_literal = if let ExprKind::Call { args, .. } = &value.kind {
                            matches!(args.first().map(|a| &a.value.kind), Some(ExprKind::Bool(_)))
                        } else {
                            false
                        };
                        if annotation_is_bool || arg_is_bool_literal {
                            self.atomic_var_inner_is_bool.insert(var_name.clone());
                        }
                    }
                    // Debugger Contract slice 5: register `let v =
                    // Runtime.list_par_blocks()` / `Runtime.list_tasks()`
                    // as a Vec-shaped binding so subsequent `v.len()` /
                    // `v.is_empty()` etc. dispatch through `compile_vec_method`.
                    // The element type is opaque from codegen's perspective
                    // (the baked-stdlib `ParBlockInfo` / `TaskInfo` structs
                    // aren't in `program.items` — see compile_program line
                    // 2720+). Using `i64` as a placeholder element type
                    // keeps Vec dispatch working for the v1 contract
                    // surface (`.len()` / `.is_empty()` ignore element
                    // type). Field access (`pb.spawn_site_id`) is a v1.x
                    // follow-up that requires registering the baked struct
                    // types.
                    if !detected && self.is_runtime_introspection_call(value) {
                        self.vec_elem_types
                            .insert(var_name.clone(), self.context.i64_type().into());
                    }
                    // Infer Slice element type from RHS shapes that produce
                    // a slice: `x.as_slice()` / `x.as_slice_mut()` on a known
                    // sequence variable, and `x[a..b]` range indexing.
                    if !self.slice_elem_types.contains_key(var_name.as_str()) {
                        if let Some(elem) = self.infer_slice_elem_from_rhs(value) {
                            self.slice_elem_types.insert(var_name.clone(), elem);
                        }
                    }
                    // Bounds-check-elision len-alias tracking: `let n = v.len()`
                    // records `n → v`, so a later `while ... and i < n and ...`
                    // guard parsed in compile_while can resolve `n` back to
                    // `v.len()` and assert `v[i]`'s upper bound. Covers both
                    // Vec and Slice receivers (parameter slice handles bind
                    // into `slice_elem_types` alongside the Vec table).
                    // Limited to bare-identifier receivers — `v[k].len()`
                    // and other non-trivial receivers aren't tracked.
                    if let ExprKind::MethodCall {
                        object,
                        method,
                        args: method_args,
                        ..
                    } = &value.kind
                    {
                        if method == "len" && method_args.is_empty() {
                            if let ExprKind::Identifier(coll_name) = &object.kind {
                                if self.vec_elem_types.contains_key(coll_name.as_str())
                                    || self.slice_elem_types.contains_key(coll_name.as_str())
                                {
                                    self.len_alias.insert(var_name.clone(), coll_name.clone());
                                }
                            }
                        }
                    }
                }
                // SoA layout: if the binding's active layout is SoA, build the
                // SoA struct instead of the normal Vec. This is the binding
                // *site* — `seed_binding_site_layout` resolves the layout (the
                // mono `layout_subst` for a returned local seeded by a
                // return-SoA monomorph, slice 3 — `let out = Vec.new()` inside
                // an `init_grid()`-shape callee — else the `layout`-block origin
                // keyed by this name) and records it in the per-binding
                // `binding_layouts` carrier, so every downstream *use*
                // (`active_soa_layout`) reads the carrier without re-touching
                // the origin map (slice 5).
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    if let Some(soa) = self.seed_binding_site_layout(var_name) {
                        // `Vec.new()` and its presize-rewritten form
                        // `Vec.with_capacity(n)` (the `presize` lowering pass
                        // turns a counted-loop fill into a capacity hint, e.g.
                        // `init_grid`/`fan_collide`'s `while c < n { v.push(..) }`)
                        // both build a fresh SoA header. The capacity is only a
                        // hint — the SoA groups still grow lazily on push — so it
                        // is dropped here (SoA pre-sizing is a perf follow-up);
                        // what matters is the binding lowers SoA, not AoS, when
                        // its initializer was capacity-rewritten.
                        if self.is_vec_new_call(value) || self.is_vec_with_capacity_call(value) {
                            return self.compile_soa_new(var_name, &soa);
                        }
                        // Backward inference (slice 3): `let <recv> = <call>()`
                        // where `recv` is SoA and the callee returns a `Vec[E]`
                        // — monomorphize the callee to RETURN the receiving
                        // binding's layout and bind the resulting SoA struct.
                        if self.let_rhs_calls_layout_returning_fn(value) {
                            return self.compile_soa_let_from_call(var_name, &soa, value);
                        }
                    }
                }
                // Map.new(): emit karac_map_new with sizes and (stub) fn pointers.
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    if self.is_map_new_call(value)
                        && self.map_key_types.contains_key(var_name.as_str())
                    {
                        let name = var_name.clone();
                        self.compile_map_new_stmt(&name)?;
                        // Slice c-repl.B.5.3b: the early-return path for
                        // Map.new() bypasses the let-arm's snapshot
                        // capture hook. Fire it here so REPL Map[K, V]
                        // bindings get the cross-cell handle stash.
                        // No-op outside REPL mode (snapshot_capture is
                        // empty), no-op for non-primitive K/V (the
                        // classifier skips them).
                        self.try_emit_snapshot_capture(pattern);
                        return Ok(());
                    }
                }
                // Set.new(): emit karac_map_new with val_size = 0. Set[T]
                // lowers to Map[T, ()] at codegen — the C runtime handles
                // val_size = 0 correctly via `(key_size + val_size).max(1)`.
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    if self.is_set_new_call(value)
                        && self.set_elem_types.contains_key(var_name.as_str())
                    {
                        let name = var_name.clone();
                        self.compile_set_new_stmt(&name)?;
                        // Slice c-repl.B.5.3c: same plumbing as the
                        // Map.new() arm just above — Set.new() bypasses
                        // the let-arm's snapshot capture hook at the
                        // bottom of this match. Fire it here so REPL
                        // `Set[T]` bindings get the cross-cell handle
                        // stash. No-op outside REPL mode
                        // (snapshot_capture is empty), no-op for
                        // non-primitive T (the classifier skips them).
                        self.try_emit_snapshot_capture(pattern);
                        return Ok(());
                    }
                }
                // Map literal: `let m: Map[K, V] = ["k": v, ...]` (bare) or
                // `Map[k: v, ...]` (prefix). Both lower to `ExprKind::MapLiteral`.
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    if let ExprKind::MapLiteral(entries) = &value.kind {
                        if self.map_key_types.contains_key(var_name.as_str()) {
                            let name = var_name.clone();
                            let entries = entries.clone();
                            return self.compile_map_literal_stmt(&name, &entries);
                        }
                    }
                }
                // Zero-init repeat literal fast path: `let buf: Array[T, N] = [0; N]`
                // (and `[false; N]`, `[0.0; N]`, etc. — any literal-zero RHS) is
                // lowered to alloca + `llvm.memset`, bypassing the aggregate-value
                // round-trip. The standard path emits `store [N x T] zeroinitializer`
                // which LLVM's downstream codegen passes crash on at N≥80K (verified
                // SIGSEGV in `write_to_file`); the memset path is correct at any N
                // and is what LLVM would lower the aggregate store to anyway.
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    if let Some(result) =
                        self.try_emit_zero_init_array_let(var_name, value, ty.as_ref())
                    {
                        return result;
                    }
                }
                // Empty array literal: `let a: Array[T, 0] = []`. Allocate a
                // real `[0 x T]` slot from the annotation so the binding
                // coerces to a zero-length slice at call sites instead of the
                // scalar-i64 sentinel `compile_array_literal` falls back to
                // (which fails Array → Slice coercion). B-2026-06-14-30.
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    if let Some(result) =
                        self.try_emit_empty_array_let(var_name, value, ty.as_ref())
                    {
                        return result;
                    }
                }
                // Prefer the explicit type annotation when present — it lets
                // `let c: Cm = i.into();` (lowered to `Cm.from(i)`, which
                // `type_name_of` can't classify) still register `c` as a
                // `Cm` so field accesses resolve.
                let type_hint = ty
                    .as_ref()
                    .and_then(|te| match &te.kind {
                        TypeKind::Path(p) => p.segments.last().cloned(),
                        _ => None,
                    })
                    .or_else(|| self.type_name_of(value))
                    .or_else(|| {
                        // Fallback: typechecker's recorded surface type for
                        // this binding (set by `bind_pattern_types` for
                        // `Type::Shared(name)`). Lets `let cur = nodes[0]`
                        // (RHS is an Index, which `type_name_of` doesn't
                        // classify) still surface "TreeNode" so the
                        // rc_inc / scope-cleanup machinery below fires,
                        // and the shared-struct copy preserves refcount
                        // discipline across mutable rebindings.
                        if let PatternKind::Binding(var_name) = &pattern.kind {
                            let key = (pattern.span.offset, pattern.span.length);
                            if let Some(surface) = self.pattern_binding_types.get(&key) {
                                if self.shared_types.contains_key(surface) {
                                    let _ = var_name;
                                    return Some(surface.clone());
                                }
                            }
                        }
                        None
                    });
                self.pending_closure_fn_type = None;
                // Skip receive-side `rc_inc` when the RHS already delivers
                // a freshly-owned ref:
                //   * `StructLiteral` — `emit_rc_alloc` initializes rc=1.
                //   * `Call` / `MethodCall` (free fn, assoc fn, method,
                //     shared-enum variant constructor) — the callee
                //     transfers +1 to the caller via the return value.
                //     The function-return handshake is: any callee
                //     returning a `shared` type hands the caller a ref
                //     that is already +1 above what the caller previously
                //     held. The bug #7 fix at
                //     `call_dispatch.rs::suppress_source_vec_cleanup_for_arg`
                //     emits the inc inside the callee at each move-out
                //     site; the source's queued scope-exit `rc_dec` then
                //     decrements its own slot back to construction-time,
                //     leaving a net +1 attached to the returned pointer.
                //     The caller therefore must NOT inc again on receive
                //     — doing so doubles the refcount and leaks one ref
                //     on every shared-struct return crossing (the
                //     receiver's scope-exit dec drops rc to 1, never 0,
                //     so `free` never fires).
                //
                // `Identifier`, `FieldAccess`, `Index`, … RHS shapes
                // still alias an existing tracked ref and need the inc.
                // Recurses into `If` / `Match` / `IfLet` / `Block` /
                // `LabeledBlock` / `Unsafe` tails — `rhs_yields_fresh_ref`
                // returns true only when every branch tail is itself a
                // fresh-ref source. Plain `Call` / `MethodCall` /
                // `StructLiteral` match the base case directly.
                let is_fresh_construction = self.rhs_yields_fresh_ref(value);
                let rhs_is_fstring = self.rhs_stages_fstr_acc(value);
                // Thread the binding's Vec element type through to
                // `Vec.with_capacity(n)` in the RHS — the zero-arg
                // constructor can't recover `T` from arguments, but
                // `vec_elem_types[var_name]` is already populated above
                // from the annotation (or pattern_binding_inner_types
                // for the no-annotation path). Cleared after compile.
                let saved_pending_let_elem = self.pending_let_elem_type.take();
                let saved_pending_let_elem_te = self.pending_let_elem_type_expr.take();
                // Sibling threading for `Tensor.zeros/ones/full` in the
                // RHS — those constructors can't recover the element
                // type or rank from their `dims: Vec[i64]` argument;
                // `tensor_var_infos[var_name]` was populated above from
                // the annotation. Cleared after compile.
                let saved_pending_let_tensor = self.pending_let_tensor_info.take();
                // Sibling threading for `Column.new/with_capacity/from_vec`
                // — `new`/`with_capacity` carry no element value in their
                // args; `column_var_infos[var_name]` was populated above
                // from the annotation. Cleared after compile.
                let saved_pending_let_column = self.pending_let_column_info.take();
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    if let Some(&elem_ty) = self.vec_elem_types.get(var_name.as_str()) {
                        self.pending_let_elem_type = Some(elem_ty);
                    }
                    // TypeExpr sibling — lets `Vec.filled` deep-clone heap-backed
                    // slot values (`Vec[Vec[_]]` / `Vec[String]`).
                    if let Some(te) = self.var_elem_type_exprs.get(var_name.as_str()) {
                        self.pending_let_elem_type_expr = Some(te.clone());
                    }
                    // Fallible-allocation constructor companions: a binding
                    // `let r: Result[Vec[T], AllocError] = Vec.try_with_capacity(n)`
                    // registers as a `Result`, not a `Vec`, so the lookup above
                    // doesn't carry `T`. Recover the element type from the
                    // annotation's Ok payload so the zero-arg fallible
                    // constructor in the RHS can size its allocation. The
                    // `?`-unwrap form (`let v: Vec[T] = try_with_capacity(n)?`)
                    // already has `T` via `vec_elem_types`, so this only fires
                    // for the match form. (phase-8-stdlib-floor item 8.)
                    if self.pending_let_elem_type.is_none() {
                        if let Some(elem_ty) = ty
                            .as_ref()
                            .and_then(|t| self.result_ok_collection_elem_type(t))
                        {
                            self.pending_let_elem_type = Some(elem_ty);
                        }
                    }
                    if let Some(info) = self.tensor_var_infos.get(var_name.as_str()) {
                        self.pending_let_tensor_info = Some(info.clone());
                    }
                    if let Some(info) = self.column_var_infos.get(var_name.as_str()) {
                        self.pending_let_column_info = Some(*info);
                    }
                }
                // Type-changing shadow dance (step 2 of 3 — old tags for the
                // RHS). The pending-let derivation above has already read the
                // NEW binding's element/tensor info from the maps into locals;
                // now stash the NEW per-variable metadata and reinstate the OLD
                // so the RHS sees the previous binding's class while it
                // compiles — `let s = s.to_vec()` (String→Vec) must dispatch
                // `s` as the old String. No-op for a non-shadow let.
                let shadow_new_meta = shadow_name.as_ref().map(|n| {
                    let new_meta = self.take_var_metadata(n);
                    if let Some(old) = shadow_old_meta.take() {
                        self.restore_var_metadata(n, old);
                    }
                    new_meta
                });
                let val = self.compile_expr(value)?;
                // Type-changing shadow dance (step 3 of 3 — pure-new tags).
                // The RHS is compiled; drop the OLD metadata and reinstate the
                // NEW binding's class tags so every later use of the new
                // binding — and the scope-exit drop registration via the
                // `track_*` calls below — dispatches correctly. The bind at the
                // bottom of the arm runs under `suppress_shadow_metadata_purge`
                // so `bind_pattern` does not re-purge what was just installed.
                if let (Some(n), Some(new_meta)) = (shadow_name.as_ref(), shadow_new_meta) {
                    self.forget_var_metadata(n);
                    self.restore_var_metadata(n, new_meta);
                }
                self.pending_let_elem_type = saved_pending_let_elem;
                self.pending_let_elem_type_expr = saved_pending_let_elem_te;
                self.pending_let_tensor_info = saved_pending_let_tensor;
                self.pending_let_column_info = saved_pending_let_column;
                // `let w = v[i]` over a heap-element `Vec` — deep-clone the shallow
                // element so the binding owns a distinct buffer; without it both
                // the binding's drop and `v`'s element-drop free the same buffer
                // (double-free, B-2026-06-14-11). No-op for every other RHS shape.
                //
                // Borrow-elision (B-2026-06-19-6): when the conservative
                // `compute_vec_index_borrow_spans` pre-pass proved this exact
                // `v[i]` binding is read-only, non-escaping, and `v` is not
                // mutated in the binding's scope, SKIP the clone — the binding
                // aliases the container element — and remember to also skip the
                // scope-exit `track_vec_*` below so the container stays the
                // unique owner (no double-free, no leak).
                let borrow_elided = matches!(&value.kind, ExprKind::Index { .. })
                    && self
                        .vec_index_borrow_spans
                        .contains(&crate::resolver::SpanKey::from_span(&value.span));
                let val = if borrow_elided {
                    val
                } else {
                    self.clone_owned_vec_index_element(value, val)?
                };
                // Owned String/Vec PARAM moved into a local binding
                // (`let mut work = lists;` where `lists` is a bare
                // by-value param): under the owned-param ABI the CALLER
                // retains the buffer's free (kata-22 family, baa210e2),
                // so arming the new binding as owner over the same
                // buffer double-frees at the two scope exits — surfaced
                // by kata-23's `merge_k_lists` (param move + in-place
                // interval merge over `Vec[Option[ListNode]]`); whether
                // it trapped or passed silently was allocator luck.
                // Deep-copy instead: the binding owns the copy, the
                // caller frees the original. The let-move suppression
                // below is skipped for this shape — the param's header
                // must stay intact (cap > 0) so any LATER retaining
                // consume site of the same param still sees an owned
                // buffer to copy.
                let rhs_is_owned_param = matches!(
                    &value.kind,
                    ExprKind::Identifier(n) if self.owned_vecstr_params.contains(n.as_str())
                );
                // B-2026-07-05-2 sibling (Vec/String leg): a `Vec[String]` /
                // `Vec[Vec[T]]` for-loop element moved WHOLE into a local
                // (`for s in words { let x = s }`). `s` aliases the container's
                // element buffer; `maybe_defensive_copy_param_arg` gives `x` an
                // independent copy (it already keys on `for_loop_borrow_vars`).
                // The struct/enum legs are handled by the aggregate deep-copy
                // hooks; this covers the Vec/String element type, which the
                // B-2026-07-04-17 struct fix did not touch — only push/insert/
                // entry consume sites were covered, not the plain whole-move
                // let-bind. The move-suppression below is skipped for this shape
                // (like the owned-param case): the loop element's slot must stay
                // intact so the container — the single owner — frees the
                // original exactly once.
                let rhs_is_for_loop_borrow_vecstr = matches!(
                    &value.kind,
                    ExprKind::Identifier(n)
                        if self.for_loop_borrow_vars.contains(n.as_str())
                            && self.vec_elem_types.contains_key(n.as_str())
                );
                let rhs_retains_own_copy = rhs_is_owned_param || rhs_is_for_loop_borrow_vecstr;
                let val = if rhs_retains_own_copy {
                    self.maybe_defensive_copy_param_arg(value, val)
                } else {
                    val
                };
                // Sibling to the Assign arm's f-string staged-acc capture.
                // The slot is consumed below at the tracked-Vec/String let-
                // binding site (it transfers ownership of the buffer to
                // the new binding's slot). Always take so a stale slot
                // can't leak into an unrelated downstream Let/Assign.
                let staged_fstr_acc = if rhs_is_fstring {
                    self.last_fstr_acc.take()
                } else {
                    None
                };
                // Track variable → type name for field resolution.
                let mut shared_info: Option<(String, SharedTypeInfo<'ctx>)> = None;
                if let Some(ref type_name) = type_hint {
                    if let PatternKind::Binding(var_name) = &pattern.kind {
                        self.var_type_names
                            .insert(var_name.clone(), type_name.clone());
                        if let Some(info) = self.shared_types.get(type_name.as_str()).cloned() {
                            shared_info = Some((var_name.clone(), info));
                        }
                    }
                }
                // `Option[shared T]` detection — peer to `shared_info`,
                // but for an Option-wrapped shared ref. Populated from:
                //   (a) explicit `let x: Option[ShareT] = ...;` annotation;
                //   (b) untyped lets whose RHS is a free-fn call returning
                //       `Option[shared T]` (recorded by `declare_function`
                //       in `fn_return_option_inner_shared`).
                // Methods / 2-segment Path calls / nested control-flow
                // tails are out of scope for this slice — the kata's
                // bench shape uses the bare-Identifier call form. When
                // populated, queues an `RcDecOption` cleanup below so the
                // inner shared ref drops on scope exit. Closes the
                // 2026-05-17 kata-bench retention bug (`let out =
                // add_two_numbers(...)` leaked one 100-node chain per
                // iter at K=500_000).
                let mut shared_option_info: Option<(String, SharedTypeInfo<'ctx>)> = None;
                // Set by case (d) below: the RHS aliases an existing
                // `Option[shared T]` binding, so the new binding is a second
                // owner of the same chain and must inc the inner ref (the
                // scope-exit `RcDecOption` queued by `track_rc_option_var`
                // would otherwise over-decrement). Cases (a)/(b)/(c) don't
                // set it — annotation/call/field RHS already deliver an owned
                // or balanced ref (a Call move-out, or the field-read's
                // balancing inc in `compile_field_access`).
                let mut option_alias_needs_inner_inc = false;
                if shared_info.is_none() {
                    if let PatternKind::Binding(var_name) = &pattern.kind {
                        // (a) Explicit annotation.
                        if let Some(te) = ty.as_ref() {
                            if let Some((inner_name, info)) =
                                self.option_inner_shared_type_for_type_expr(te)
                            {
                                shared_option_info = Some((var_name.clone(), info));
                                // Mirror `shared_info`'s var-type-names
                                // contract: record the OUTER name ("Option")
                                // so downstream resolvers see this binding
                                // as an Option; the inner shared name
                                // travels through `shared_option_info`
                                // alone (not surfaced via var_type_names
                                // to avoid masking the Option-ness).
                                let _ = inner_name;
                            }
                        }
                        // (b) Untyped let with a call-shaped RHS whose
                        //     return type is `Option[shared T]`. The
                        //     declare_function pass recorded the inner
                        //     shared name in `fn_return_option_inner_shared`
                        //     (keyed by LLVM symbol: bare name for free
                        //     fns, `Type.method` for impl methods).
                        //     Covered shapes (extended 2026-06-05 for the
                        //     niche-ABI method slice — previously
                        //     free-fn-Identifier only, which left
                        //     `let entry = cache.lookup(k)`-style bindings
                        //     unregistered: `is_some`/`unwrap` dispatch
                        //     fell through and no `RcDecOption` was
                        //     queued):
                        //       - `f(...)`            → key `f`
                        //       - `Type.assoc(...)`   → key `Type.assoc`
                        //       - `Resource.m(...)`   → representative
                        //         impl key via `provider_method_impl_key`
                        //         (the callee symbol is a vtable slot,
                        //         not a declared fn)
                        //       - `obj.method(...)`   → key
                        //         `<receiver type>.method` via
                        //         `inferred_receiver_type` (builtin
                        //         receivers like Vec/Map produce keys
                        //         absent from the map — no false
                        //         positives)
                        if shared_option_info.is_none() {
                            let callee_key: Option<String> = match &value.kind {
                                ExprKind::Call { callee, .. } => match &callee.kind {
                                    ExprKind::Identifier(fn_name) => Some(fn_name.clone()),
                                    ExprKind::Path { segments, .. } if segments.len() == 2 => {
                                        let direct = format!("{}.{}", segments[0], segments[1]);
                                        if self.fn_return_option_inner_shared.contains_key(&direct)
                                        {
                                            Some(direct)
                                        } else {
                                            self.provider_method_impl_key(
                                                &segments[0],
                                                &segments[1],
                                            )
                                        }
                                    }
                                    _ => None,
                                },
                                ExprKind::MethodCall { object, method, .. } => self
                                    .inferred_receiver_type(object)
                                    .map(|t| format!("{}.{}", t, method)),
                                _ => None,
                            };
                            if let Some(key) = callee_key {
                                if let Some(inner_name) = self
                                    .fn_return_option_inner_shared
                                    .get(key.as_str())
                                    .cloned()
                                {
                                    if let Some(info) =
                                        self.shared_types.get(inner_name.as_str()).cloned()
                                    {
                                        shared_option_info = Some((var_name.clone(), info));
                                    }
                                }
                            }
                        }
                        // (c) Untyped let with a FieldAccess RHS where
                        //     the object is a call-like (or
                        //     Identifier-bound) shared struct and the
                        //     accessed field's declared type is
                        //     `Option[shared T]`. Covers
                        //     `let v = get_node().next;` (call-chain
                        //     FieldAccess) and `let v = obj.next;`
                        //     where `obj` is a tracked shared-struct
                        //     binding and `next` is the Option-shared
                        //     field. Recovers the inner heap type by
                        //     walking from the object's static
                        //     struct name through
                        //     `struct_field_type_exprs` to the field's
                        //     full TypeExpr, then dispatching through
                        //     `option_inner_shared_type_for_type_expr`.
                        //     Without this, the field-access's
                        //     `compile_field_access` call-chain branch
                        //     inc's the inner ref to balance the temp
                        //     drop (slice 3's other half), but the
                        //     let-binding never queues an RcDecOption
                        //     cleanup — the chain stays live
                        //     forever, one per iteration leak.
                        if shared_option_info.is_none() {
                            if let ExprKind::FieldAccess { object, field } = &value.kind {
                                let call_like_name: Option<String> =
                                    self.shared_type_for_call_like(object).map(|(n, _)| n);
                                // Identifier/self-bound object (vs call-like):
                                // the field read is a bare load — the
                                // call-chain branch of `compile_field_access`
                                // emits its own balancing inc for call-like
                                // objects, but the binding-object read does
                                // NOT. The new binding is a second owner of
                                // the field's chain, so flag the same
                                // aliasing-acquire inner inc case (d) uses;
                                // the queued `RcDecOption` balances it.
                                // Without this, `let stepped = node.next;`
                                // queued an unbalanced dec: stepped's
                                // scope-exit dec freed the sub-chain the
                                // field still owned, and the owner's later
                                // drop walked freed memory — a LATENT
                                // double-free on main since case (c) landed,
                                // masked because the freed chunk's garbage
                                // rc-word usually stops the walk; the
                                // niche-ABI slice's allocation-pattern shift
                                // made it trap deterministically (v0c repro,
                                // 2026-06-05).
                                let via_binding = call_like_name.is_none();
                                let obj_type_name: Option<String> = call_like_name
                                    .or_else(|| self.shared_type_for_expr(object).map(|(n, _)| n));
                                if let Some(type_name) = obj_type_name {
                                    if let Some(idx) = self
                                        .struct_field_names
                                        .get(&type_name)
                                        .and_then(|names| names.iter().position(|n| n == field))
                                    {
                                        if let Some(field_te) = self
                                            .struct_field_type_exprs
                                            .get(&type_name)
                                            .and_then(|v| v.get(idx))
                                            .cloned()
                                        {
                                            if let Some((_, info)) = self
                                                .option_inner_shared_type_for_type_expr(&field_te)
                                            {
                                                shared_option_info = Some((var_name.clone(), info));
                                                if via_binding {
                                                    option_alias_needs_inner_inc = true;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        // (d) Untyped let aliasing another `Option[shared T]`
                        //     binding: `let mut a = l1;` where `l1` is a tracked
                        //     Option[shared] parameter / binding (registered in
                        //     `var_option_shared_heap`). Without this, `a` is
                        //     never registered, so the cursor reassignment
                        //     `a = na.next;` finds no `var_option_shared_heap`
                        //     entry and performs ZERO refcount management — the
                        //     node `a` advances onto is never retained and is
                        //     freed when a later splice (`tail.next = Some(nb)`)
                        //     overwrites the prior node's `.next` (the niche
                        //     field store releases the displaced inner). The
                        //     node is still reachable through `a` but its ref
                        //     was never counted → use-after-free. This is the
                        //     merge-two-sorted-lists (LeetCode #21) cursor idiom.
                        //
                        //     `a` becomes a second owner of `l1`'s chain, so the
                        //     inner ref must be inc'd here (flagged); the
                        //     scope-exit `RcDecOption` `track_rc_option_var`
                        //     queues for `a` balances it. The reverse-lookup of
                        //     `shared_types` by `heap_type` only consumes
                        //     `info.heap_type` (used by `track_rc_option_var`);
                        //     since structurally-equal anonymous heap layouts
                        //     compare equal, the resolved `heap_type` is correct
                        //     regardless of which same-layout name is picked —
                        //     the same reverse-lookup `track_rc_option_var`
                        //     itself already relies on via `struct_name_for_heap_type`.
                        if shared_option_info.is_none() {
                            if let ExprKind::Identifier(rhs_name) = &value.kind {
                                if let Some(heap_type) =
                                    self.var_option_shared_heap.get(rhs_name.as_str()).copied()
                                {
                                    if let Some(info) = self
                                        .shared_types
                                        .values()
                                        .find(|i| i.heap_type == heap_type)
                                        .cloned()
                                    {
                                        shared_option_info = Some((var_name.clone(), info));
                                    }
                                }
                            }
                        }
                        // Aliasing acquire: when the RHS is an Identifier naming
                        // an existing `Option[shared T]` binding, the new
                        // binding is a SECOND owner of that chain and must inc
                        // the inner ref. Fires for both the annotated path
                        // (case a, `let a: Option[T] = l1`) and the untyped
                        // path (case d, `let mut a = l1`). Cases b/c (Call /
                        // FieldAccess RHS) are excluded by the Identifier check —
                        // a Call move-out already owns its ref, and the field
                        // read in `compile_field_access` emits its own balancing
                        // inc. Gated on `shared_option_info` being resolved so
                        // the inner heap type is known.
                        if shared_option_info.is_some() {
                            if let ExprKind::Identifier(rhs_name) = &value.kind {
                                if self.var_option_shared_heap.contains_key(rhs_name.as_str()) {
                                    option_alias_needs_inner_inc = true;
                                }
                            }
                        }
                    }
                }
                // Fallback: when there is no type annotation and the RHS is a
                // call (or any expression `type_name_of` can't classify), but
                // the compiled value is a struct, recover the user-type name
                // from the source AST when possible (UFCS shape `Target.fn(...)`
                // where the receiver is the target type's name) before falling
                // back to LLVM-struct-identity reverse-lookup. Lets
                // `let f = Foo.default()` populate `var_type_names` so
                // `f.value` resolves correctly — and also disambiguates
                // distinct user types that lower to the same LLVM struct
                // shape (e.g. two providers each `{ i64 }`), which the bare
                // LLVM-identity reverse-lookup would alias by HashMap-iteration
                // order. See `bugs.md` entry "Provider struct identity
                // collision in codegen's `var_type_names`".
                if type_hint.is_none() {
                    if let (BasicValueEnum::StructValue(sv), PatternKind::Binding(var_name)) =
                        (&val, &pattern.kind)
                    {
                        let st = sv.get_type();
                        // Prefer source-AST identity for UFCS associated-fn calls
                        // whose target is a known user struct and whose LLVM
                        // return type matches that struct's LLVM identity.
                        let ast_hint = match &value.kind {
                            ExprKind::Call { callee, .. } => {
                                if let ExprKind::Path { segments, .. } = &callee.kind {
                                    if segments.len() == 2 {
                                        let target = &segments[0];
                                        // `Stats.min`/`max` → `Option[f64]`,
                                        // `Stats.argmin`/`argmax` → `Option[i64]`
                                        // (intercepted in `try_compile_stats_call`,
                                        // no user-fn return-type entry) — record the
                                        // binding as `Option` so a downstream `match`
                                        // can resolve the scrutinee's enum and bind
                                        // the payload.
                                        if target == "Stats"
                                            && matches!(
                                                segments[1].as_str(),
                                                "min" | "max" | "argmin" | "argmax"
                                            )
                                        {
                                            Some("Option".to_string())
                                        } else {
                                            match self.struct_types.get(target) {
                                                Some(target_st) if *target_st == st => {
                                                    Some(target.clone())
                                                }
                                                _ => None,
                                            }
                                        }
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                }
                            }
                            // A struct literal names its type authoritatively in
                            // source — use it directly. Crucial for distinct
                            // structs that lower to the same LLVM shape (every
                            // empty struct is `{}`, e.g. `StdoutExporter` vs
                            // `NoOpExporter`), which the LLVM-identity reverse-
                            // lookup below would alias by HashMap-iteration order.
                            ExprKind::StructLiteral { path, .. } => path
                                .last()
                                .filter(|n| self.struct_types.contains_key(n.as_str()))
                                .cloned()
                                .or_else(|| {
                                    // Enum struct-variant construction
                                    // `Enum.Variant { ... }` (parsed as a
                                    // StructLiteral whose `path[len-2]` is the
                                    // enum and `path.last()` is the variant):
                                    // bind the let as the ENUM, not the variant
                                    // (which is not a `struct_types` key). Lets
                                    // `expr_user_enum_name`/`_any` dispatch
                                    // Display + method calls on an unannotated
                                    // `let b = Shape.Rect { .. }` binding.
                                    if path.len() >= 2 {
                                        let enum_name = &path[path.len() - 2];
                                        if self.enum_layouts.contains_key(enum_name) {
                                            return Some(enum_name.clone());
                                        }
                                    }
                                    None
                                }),
                            // A method-call RHS returning a struct (`let b =
                            // w.bump()` where `bump -> Self`): resolve the
                            // concrete return-type name via `type_name_of_expr`
                            // (which reads the receiver's registered type +
                            // `fn_return_type_names`), guarded by an LLVM-shape
                            // match so a stale name can't mislabel. Without this
                            // the binding fell to the reverse-lookup below, which
                            // picks the FIRST same-shape struct in HashMap order
                            // (e.g. a `{i64}` `Ctr` aliased to `TcpStream`) —
                            // the binding then dispatched `b.method()` against
                            // the wrong type. Surfaced by a `-> Self` method
                            // called through a generic bound (B-2026-07-03-11).
                            ExprKind::MethodCall { .. } => self
                                .type_name_of_expr(value)
                                .filter(|n| matches!(self.struct_types.get(n.as_str()), Some(t) if *t == st)),
                            _ => None,
                        };
                        if let Some(name) = ast_hint {
                            self.record_var_type_name(var_name.clone(), name);
                        } else if !self.var_type_names.contains_key(var_name.as_str()) {
                            // LLVM-struct-identity reverse-lookup fallback.
                            // Only fires when `pattern_binding_types`
                            // (read above via line ~781 and written to
                            // `var_type_names` at line ~817) hasn't
                            // already populated the entry — otherwise
                            // this would override the typechecker's
                            // authoritative answer with a HashMap-
                            // iteration-order pick when multiple
                            // seeded structs share the same LLVM shape
                            // (e.g. `RequestBuilder { handle: i64 }`
                            // vs `TaskGroup { id: i64 }`, both `{i64}`).
                            // Phase-8 line 24 guard, 2026-05-29.
                            //
                            // Phase 5 line 569 slice 4: union RHS
                            // recognition. Union literals return a
                            // StructValue of the union's storage type;
                            // without the union arm the let-bound
                            // binding never lands in `var_type_names`
                            // and downstream `u.field` codegen can't
                            // route through the union-aware field-
                            // access path.
                            if let Some((name, _)) =
                                self.struct_types.iter().find(|(_, ty)| **ty == st)
                            {
                                let name = name.clone();
                                self.record_var_type_name(var_name.clone(), name);
                            } else if let Some((name, _)) =
                                self.union_types.iter().find(|(_, ty)| **ty == st)
                            {
                                let name = name.clone();
                                self.record_var_type_name(var_name.clone(), name);
                            }
                        }
                    }
                }
                // `?` on `Option[shared T]` (`let first = head?;`) yields the
                // unwrapped payload as the raw i64 word `q_w0` — the enum
                // payload lowering is word-uniform (`compile_question` hands
                // back field 1 untyped). A shared binding's slot must hold
                // the heap pointer: int_to_ptr it back before the inc/track
                // below and before the alloca takes `val`'s type, so
                // downstream field access / method dispatch see the pointer
                // shape every other shared RHS produces. Pre-existing gap
                // (panicked at `into_pointer_value` on any karac build since
                // the `?` lowering landed) surfaced 2026-06-05 by the
                // niche-ABI slice's `?` convergence test; `.unwrap()` was
                // never affected — its method lowering re-types the payload.
                let val = if shared_info.is_some() && val.is_int_value() {
                    self.builder
                        .build_int_to_ptr(
                            val.into_int_value(),
                            self.context.ptr_type(inkwell::AddressSpace::default()),
                            "shared_w0_ptr",
                        )
                        .unwrap()
                        .into()
                } else {
                    val
                };
                // For shared types: rc_inc when copying from another variable (not fresh construction).
                if let Some((ref var_name, ref info)) = shared_info {
                    // Phase-B2 non-owning roles (fresh nodes, bare
                    // cursors): NO receive-inc and NO cleanup — the
                    // chain owns the object and the root's free-walk is
                    // the single release point. Nothing is freed before
                    // scope exit in a b2 cluster (displacement-free
                    // shapes only), so count-free aliases never dangle.
                    let b2_skip = self.b2_skips_counts(var_name);
                    if !is_fresh_construction && !b2_skip {
                        // Copying a shared pointer — increment refcount.
                        let ptr = val.into_pointer_value();
                        self.emit_refcount_inc(var_name, info.heap_type, ptr);
                    }
                    // Track for scope-exit cleanup. RC-elided bindings
                    // (ownership phase-A elision — refcount provably
                    // never exceeds 1, no heap fields, no user Drop)
                    // queue an unconditional free instead of the
                    // dec/zero-test/drop walk. The analysis only elides
                    // struct-literal births, so the `!is_fresh` inc arm
                    // above never fires for them.
                    let ptr = val.into_pointer_value();
                    if self.is_elided_binding(var_name) {
                        self.track_elided_shared_var(var_name, ptr);
                    } else if let Some((member_type, link_idx, returned)) =
                        self.cluster_root_info(var_name)
                    {
                        use crate::ownership::ReturnedChain;
                        match returned {
                            // Phase-B1 cluster root: link-following
                            // free-walk instead of the dec/drop-fn walk.
                            ReturnedChain::No => {
                                self.track_cluster_root_var(var_name, ptr, &member_type, link_idx);
                            }
                            // C1b RootLink: the chain transfers out via
                            // the sanctioned tail link read; only the
                            // root header node itself is freed at scope
                            // exit. FreeSharedElided is exactly that
                            // shape (reload by name, null-guard, free —
                            // no dec, no field walk).
                            ReturnedChain::RootLink => {
                                self.track_elided_shared_var(var_name, ptr);
                            }
                            // C1b SomeRoot: the entire cluster
                            // transfers to the caller at rc==1 per node
                            // (b2 count-free build) — no cleanup at all.
                            ReturnedChain::SomeRoot => {}
                        }
                    } else if !b2_skip {
                        self.track_rc_var(var_name, ptr, info.heap_type);
                    }
                }
                // RC-fallback boxing: heap-box non-shared bindings flagged by the ownership checker.
                // Skipped for Vec/String bindings (their inner buffers need separate cleanup),
                // and for `Option[shared T]` bindings: the inner node is already RC-managed
                // (capture-inc / assign inc-dec / scope-exit RcDecOption), and every
                // `var_option_shared_heap` codegen path GEPs the binding's slot as a raw
                // 4-word Option struct — boxing redirects the slot to a `{rc, Option}` heap
                // ptr those paths know nothing about, so the Option-assign arm smashes the
                // 8-byte slot with a 32-byte store (prepend-builder `head = Some(node)`
                // segfault) and the tag reads decode a heap address as the discriminant.
                let val = if let PatternKind::Binding(var_name) = &pattern.kind {
                    let is_vec = self.vec_elem_types.contains_key(var_name.as_str());
                    if shared_info.is_none()
                        && shared_option_info.is_none()
                        && !is_vec
                        && self.is_rc_fallback_binding(var_name)
                    {
                        let val_ty = val.get_type();
                        let heap_type = self
                            .context
                            .struct_type(&[self.context.i64_type().into(), val_ty], false);
                        let heap_ptr = self.emit_rc_alloc(heap_type);
                        let val_field = self
                            .builder
                            .build_struct_gep(heap_type, heap_ptr, 1, "rc_fb_val")
                            .unwrap();
                        self.builder.build_store(val_field, val).unwrap();
                        self.rc_fallback_heap_types
                            .insert(var_name.clone(), heap_type);
                        // When the boxed value is an aggregate (tuple / struct)
                        // with String/Vec fields, synthesize a value-drop fn so
                        // the box free at rc==0 recurses into those buffers
                        // instead of leaking them (B-2026-06-10-8). No-op for
                        // scalar / heap-free boxed values.
                        self.register_rc_fallback_box_drop(heap_type);
                        self.track_rc_var(var_name, heap_ptr, heap_type);
                        heap_ptr.into()
                    } else {
                        val
                    }
                } else {
                    val
                };
                // Register closure function type under bound names.
                if let Some(fn_type) = self.pending_closure_fn_type.take() {
                    for bound_name in pattern.binding_names() {
                        self.closure_fn_types.insert(bound_name, fn_type);
                    }
                }
                // Slice pattern let — `let [a, b, c] = arr;`. The
                // value-based `bind_pattern` fall-through would no-op;
                // route through the SliceSource helper so prefix/suffix
                // sub-patterns and the rest binding land correctly.
                if let PatternKind::Slice {
                    prefix,
                    rest,
                    suffix,
                } = &pattern.kind
                {
                    let src = self.resolve_slice_source(value).ok_or_else(|| {
                        "slice pattern requires an identifier RHS resolvable to Array/Vec/Slice"
                            .to_string()
                    })?;
                    self.bind_slice_pattern(prefix, rest, suffix, &src, false)?;
                } else {
                    // For a type-changing shadow the dance above already
                    // installed the new binding's pure metadata; tell
                    // `bind_pattern` not to re-purge it. Non-shadow lets and
                    // destructures leave the flag false (the latter rely on
                    // `bind_pattern`'s purge + their own post-bind
                    // re-registration). Reset before the `?` so an error can't
                    // leak the flag into later compilation.
                    self.suppress_shadow_metadata_purge = shadow_name.is_some();
                    let bind_res = self.bind_pattern(pattern, val);
                    self.suppress_shadow_metadata_purge = false;
                    bind_res?;
                    // `let Point { items, count } = …` — `bind_pattern` only
                    // allocas the field bindings; it registers neither method
                    // dispatch nor scope-exit cleanup for them, so destructured
                    // heap fields used to be undispatchable (`items.len()` →
                    // "no handler for method") AND leaked. Wire both here (B
                    // follow-up #3 / docs/spikes/pattern-arm-unbound-field-drop.md).
                    if matches!(&pattern.kind, PatternKind::Struct { .. }) {
                        self.finish_owned_struct_destructure(pattern, value, val)?;
                    }
                    // B-2026-06-13-5: `let (a, b) = pair()` — `bind_pattern`'s
                    // Tuple arm extracts each element into a fresh leaf alloca
                    // but registers no scope-exit free, so a String/Vec element's
                    // heap buffer leaked (2000 leaks / 46 KB over a 1000-iter
                    // destructure loop). Tuple counterpart of the struct call
                    // above; dispatch was already wired in `bind_pattern` (B-12-3).
                    if matches!(&pattern.kind, PatternKind::Tuple(_)) {
                        self.finish_owned_tuple_destructure(pattern, value, val)?;
                    }
                }
                // For shared-struct lets that may not execute at runtime
                // (nested inside a loop body or conditional branch),
                // null-init the slot at function entry. The bind_pattern
                // store above runs in the let-stmt's basic block; when
                // that block is unreachable at runtime, the alloca
                // stays at the entry-block null sentinel emitted here,
                // and the cleanup walker's null-guard (in
                // `emit_cleanup_action`'s `RcDec` arm) skips the dec.
                // Mirrors the way Rust's MIR Drop-flag tracking encodes
                // conditionally-live bindings.
                //
                // Skipped when the let is at function top-level (its
                // bind_pattern store always runs, so the null sentinel
                // would just be immediate dead-store). The
                // `scope_cleanup_actions.len() > 1` check distinguishes
                // nested-block lets (their cleanup frame is the
                // function's, but they live inside a control-flow
                // sub-block whose body store may not fire) from
                // top-level lets that share the entry block. Actually,
                // all lets currently land in the function-level frame
                // (no per-block frames), so the heuristic uses the
                // builder's current basic block: if it isn't the entry
                // block, we're nested.
                if shared_info.is_some() {
                    if let PatternKind::Binding(var_name) = &pattern.kind {
                        if let Some(slot) = self.variables.get(var_name.as_str()).copied() {
                            let is_nested = self
                                .current_fn
                                .and_then(|f| f.get_first_basic_block())
                                .zip(self.builder.get_insert_block())
                                .map(|(entry, cur)| entry != cur)
                                .unwrap_or(false);
                            if is_nested {
                                self.null_init_slot_in_entry_block(slot.ptr);
                            }
                        }
                    }
                }
                // `Option[shared T]` cleanup registration. Must run
                // AFTER `bind_pattern` so the slot exists in
                // `self.variables`; same site as the plain shared
                // `track_rc_var` above. Also null-init the slot's tag
                // word when the let lives in a nested block — without
                // this, a never-executed body leaves the slot at
                // `undef` and the cleanup loads garbage as the tag,
                // potentially matching `Some` and dereferencing a
                // garbage pointer. The tag-zero sentinel maps to
                // `None`, which the cleanup arm skips. Stores zero
                // across the WHOLE Option struct rather than just tag,
                // for defense in depth — the w0/w1/w2 fields are
                // ignored on the None side anyway.
                if let Some((ref var_name, ref info)) = shared_option_info {
                    if let Some(slot) = self.variables.get(var_name.as_str()).copied() {
                        let option_ty = self.enum_layouts["Option"].llvm_type;
                        let is_nested = self
                            .current_fn
                            .and_then(|f| f.get_first_basic_block())
                            .zip(self.builder.get_insert_block())
                            .map(|(entry, cur)| entry != cur)
                            .unwrap_or(false);
                        if is_nested {
                            self.zero_init_option_slot_in_entry_block(slot.ptr, option_ty);
                        }
                        // Phase C1c adopted root: the builder-call
                        // result owns a chain at rc==1 per node — the
                        // scope-exit cleanup is the option-guarded
                        // free-walk, NOT the RcDecOption dec-walk.
                        // `var_option_shared_heap` registration is
                        // deliberately skipped: adopted roots are never
                        // reassigned (the analysis poisons that), and
                        // skipping keeps the Assign machinery + case
                        // (d) alias-acquire from ever treating family
                        // cursors as owners.
                        if let Some((member_type, link_idx)) = self.adopted_root_info(var_name) {
                            self.track_adopted_cluster_root_var(
                                var_name,
                                slot.ptr,
                                option_ty,
                                &member_type,
                                link_idx,
                            );
                        } else if !self.b2_skips_counts(var_name) {
                            self.track_rc_option_var(var_name, slot.ptr, option_ty, info.heap_type);
                            // Case (d) aliasing acquire: the new binding is a second
                            // owner of the RHS binding's chain — inc the inner ref so
                            // the just-queued scope-exit `RcDecOption` is balanced.
                            // Load the slot back (it now holds the aliased Option
                            // value) and inc its inner under the standard Some-tag +
                            // null guard.
                            if option_alias_needs_inner_inc {
                                let loaded = self
                                    .builder
                                    .build_load(option_ty, slot.ptr, "opt.alias.inc.load")
                                    .unwrap();
                                self.emit_option_inner_rc_inc_for_loaded(loaded, info.heap_type);
                            }
                        }
                    }
                }
                // Oversized boxed enum payload (`Option[Wide]` /
                // `Result[Wide, _]`) — queue a scope-exit free of the heap
                // box. The declared type names the payload `T` directly; for
                // an *untyped* let whose RHS is a known function call
                // (`let o = make_opt()`), recover `T` from the callee's
                // recorded return type (§3). Fresh-temp scrutinees
                // (`match v.pop()`) are handled at the scrutinee, not here.
                // Skipped when a shared-Option cleanup is already queued — a
                // shared payload is a 1-word RC pointer and is never boxed.
                if shared_option_info.is_none() {
                    if let PatternKind::Binding(var_name) = &pattern.kind {
                        let boxed_te: Option<TypeExpr> =
                            ty.clone().or_else(|| self.untyped_let_boxed_enum_te(value));
                        if let Some(te) = boxed_te.as_ref() {
                            let boxed = self.boxed_enum_payload_variants(te);
                            if let Some(slot) = (!boxed.is_empty())
                                .then(|| self.variables.get(var_name.as_str()).copied())
                                .flatten()
                            {
                                // Zero-init nested-scope slots so a let that
                                // doesn't execute leaves tag=0 (no payload),
                                // not undef, at cleanup — mirrors the
                                // shared-Option path above.
                                let is_nested = self
                                    .current_fn
                                    .and_then(|f| f.get_first_basic_block())
                                    .zip(self.builder.get_insert_block())
                                    .map(|(entry, cur)| entry != cur)
                                    .unwrap_or(false);
                                if is_nested {
                                    if let Some(en) = boxed.first().map(|b| b.0) {
                                        let enum_ty = self.enum_layouts[en].llvm_type;
                                        self.zero_init_option_slot_in_entry_block(
                                            slot.ptr, enum_ty,
                                        );
                                    }
                                }
                                for (enum_name, variant, inner) in &boxed {
                                    self.track_boxed_enum_var(
                                        var_name,
                                        slot.ptr,
                                        enum_name,
                                        variant,
                                        inner.as_deref(),
                                    );
                                }
                            }
                        }
                    }
                }
                // Track Tensor bindings for scope cleanup. Unannotated
                // bindings (`let t = Tensor.from(...)`, or a tensor-
                // returning call) register here from the lowering
                // side-table at the RHS span; annotated ones registered
                // above. The move-suppression call handles `let b = a;`
                // (source slot nulled — see `FreeTensor`'s null guard).
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    let key = (value.span.offset, value.span.length);
                    // A `ref Tensor` / `mut ref Tensor` RHS (e.g. a
                    // `-> ref Tensor` free-fn return) binds a BORROW: the
                    // binding is the same block pointer the owner holds, so
                    // it is registered for indexing / shape / transforms but
                    // must NOT get a `FreeTensor` (the owner frees the block;
                    // a second free would double-free). `ref_return_inner_types`
                    // carries every ref-typed expr span — its presence at the
                    // RHS span is the borrow signal.
                    let rhs_is_borrow = self.ref_return_inner_types.contains_key(&key);
                    if !self.tensor_var_infos.contains_key(var_name.as_str()) {
                        if let Some(ti) = self.tensor_typed_exprs.get(&key).cloned() {
                            let info = self.tensor_var_info_from_table(&ti);
                            self.tensor_var_infos.insert(var_name.clone(), info);
                        }
                    }
                    if self.tensor_var_infos.contains_key(var_name.as_str()) && !rhs_is_borrow {
                        if let Some(slot) = self.variables.get(var_name.as_str()) {
                            if matches!(slot.ty, BasicTypeEnum::PointerType(_)) {
                                let slot_ptr = slot.ptr;
                                self.track_tensor_var(slot_ptr);
                            }
                        }
                        self.suppress_source_vec_cleanup_for_arg(value);
                    }
                }
                // Track Column bindings for scope cleanup (phase-11
                // data-science stdlib). Unannotated bindings (a
                // column-returning call) register here from the lowering
                // side-table at the RHS span; annotated ones above. The
                // move-suppression call handles `let b = a;` (source slot
                // nulled — see `FreeColumn`'s null guard). A `ref Column`
                // RHS is a borrow and must NOT get a `FreeColumn`.
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    let key = (value.span.offset, value.span.length);
                    let rhs_is_borrow = self.ref_return_inner_types.contains_key(&key);
                    if !self.column_var_infos.contains_key(var_name.as_str()) {
                        if let Some(ci) = self.column_typed_exprs.get(&key).cloned() {
                            let info = self.column_var_info_from_table(&ci);
                            self.column_var_infos.insert(var_name.clone(), info);
                        }
                    }
                    if self.column_var_infos.contains_key(var_name.as_str()) && !rhs_is_borrow {
                        let string_elem = self
                            .column_var_infos
                            .get(var_name.as_str())
                            .is_some_and(|i| self.column_elem_is_string(i.elem));
                        if let Some(slot) = self.variables.get(var_name.as_str()) {
                            if matches!(slot.ty, BasicTypeEnum::PointerType(_)) {
                                let slot_ptr = slot.ptr;
                                self.track_column_var(slot_ptr, string_elem);
                            }
                        }
                        self.suppress_source_vec_cleanup_for_arg(value);
                    }
                }
                // Track DataFrame bindings for scope cleanup (FreeDataFrame).
                // A `let b = a;` move suppresses the source's free (its slot
                // is nulled — see FreeDataFrame's null guard).
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    if self.dataframe_var_infos.contains(var_name.as_str()) {
                        if let Some(slot) = self.variables.get(var_name.as_str()) {
                            if matches!(slot.ty, BasicTypeEnum::PointerType(_)) {
                                let slot_ptr = slot.ptr;
                                self.track_dataframe_var(slot_ptr);
                            }
                        }
                        self.suppress_source_vec_cleanup_for_arg(value);
                    }
                }
                // Track Vec variables for scope cleanup.
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    if let Some(&elem_ty) = self.vec_elem_types.get(var_name.as_str()) {
                        // B-2026-06-10-2: a Vec/String field moved OUT of a
                        // by-value struct param (`let inner = h.v`) is bound as
                        // a shallow alias of the caller's buffer (the param is a
                        // shallow struct copy). The new local is tracked for a
                        // `FreeVecBuffer` below, and the CALLER's struct-drop
                        // frees the same buffer → double-free. Deep-copy the
                        // field buffer so the moved-out local owns an
                        // independent one; the caller frees the original
                        // exactly once. Runs BEFORE `track_vec_var` so the
                        // queued free targets the copy.
                        self.deep_copy_owned_struct_param_field_move(
                            var_name.as_str(),
                            value,
                            elem_ty,
                        );
                        // #17 gap 2 / #16 — a Vec/String field moved OUT of an
                        // OWNED tracked struct: a callee-owned by-value param
                        // (#14 entry-copy + gap-1 band-aid retirement) or a LOCAL
                        // struct this fn owns. NOT a caller-retains
                        // `owned_struct_params` source — that's the deep-copy
                        // above. The moved-out binding (`var_name`, tracked just
                        // below) now owns the buffer, so cap-zero the source field
                        // in the owning struct's slot; without it BOTH the owning
                        // struct's drop and this binding free the same buffer
                        // (the std.tracing `with_field` `let nf = self.fields`
                        // shape, and the bare-local `let m = v.s` of #16).
                        if let ExprKind::FieldAccess { object, .. } = &value.kind {
                            let obj_name = match &object.kind {
                                ExprKind::Identifier(obj) => Some(obj.as_str()),
                                ExprKind::SelfValue => Some("self"),
                                _ => None,
                            };
                            if let Some(obj) = obj_name {
                                if !self.owned_struct_params.contains(obj) {
                                    self.suppress_struct_field_move_into_literal(value);
                                }
                            }
                        }
                        if let Some((slot_ptr, slot_ty)) =
                            self.variables.get(var_name.as_str()).map(|s| (s.ptr, s.ty))
                        {
                            // Copy the slot's `{ptr, ty}` out of `self.variables`
                            // up front so the immutable borrow ends before the
                            // `&mut self` drop-fn synthesis below
                            // (`vec_elem_agg_drop_for_type_expr`).
                            // Defensive guard against stale `vec_elem_types`
                            // entries for non-Vec slots — specifically, Array
                            // bindings (`let a = [1, 2, 3]` → `alloca [N x T]`).
                            // The let-binding paths above and the pattern-binding
                            // sites can both leave `vec_elem_types[name]`
                            // populated for an array-typed slot via stale
                            // typechecker classification; without this guard,
                            // `track_vec_var` queues a `FreeVecBuffer` whose
                            // scope-exit GEP treats the slot as `{ptr, i64, i64}`,
                            // reads element-2-of-the-array as "cap", finds it
                            // non-zero, GEPs out element-0 as a "data pointer",
                            // and `free()`s a non-heap value. AOT happens to
                            // print the right output BEFORE the bad free (so
                            // `Command::output()` captures stdout and the test
                            // passes); LLJIT runs in-process so the abort kills
                            // the test process — that's how this surfaced in
                            // W3.3 routing of `test_e2e_array_for_loop`. Skip
                            // the registration when the slot's LLVM type is
                            // anything but the Vec / String aggregate.
                            //
                            // Borrow-elision (B-2026-06-19-6): when the clone was
                            // skipped above, this binding aliases the container
                            // element and does NOT own a buffer — registering a
                            // `track_vec_*` cleanup would double-free (the
                            // container's drop frees the same buffer). Skip it.
                            if !borrow_elided && !matches!(slot_ty, BasicTypeEnum::ArrayType(_)) {
                                // `Vec[Tensor]` (the `iter_axis` result):
                                // elements are `ptr`s to tensor blocks that
                                // each need a `free`. The generic
                                // recursive-drop only reaches vec-struct /
                                // Map elements, so route to the
                                // tensor-element cleanup instead.
                                let elem_te =
                                    self.var_elem_type_exprs.get(var_name.as_str()).cloned();
                                let is_tensor_elem = elem_te
                                    .as_ref()
                                    .map(|te| self.tensor_var_info_from_type_expr(te).is_some())
                                    .unwrap_or(false);
                                let map_elem_drop = elem_te
                                    .as_ref()
                                    .and_then(|te| self.vec_elem_map_drop_for_type_expr(te));
                                let agg_elem_drop = elem_te
                                    .as_ref()
                                    .and_then(|te| self.vec_elem_agg_drop_for_type_expr(te));
                                // Vec-store slice (B-2026-06-22-2): a `Vec[Fn]`
                                // that OWNS heap-env closures (>=1 heap-env push,
                                // flagged in `reject_heap_env_misuse`). Free each
                                // live element's closure env via a DYNAMIC `0..len`
                                // drop loop — reusing the `elem_agg_drop` drain with
                                // a synthesized per-element env-drop fn. The Vec is
                                // homogeneous `Vec[Fn]`, so `elem_ty` is the closure
                                // fat-pointer struct (the GEP stride). Without this
                                // the element envs leak; the guard rejects any escape
                                // / non-heap-env push, so every live element is a
                                // heap-env (or null-env) closure this Vec owns.
                                let is_heap_env_vec =
                                    self.heap_env_vec_owners.contains(var_name.as_str());
                                if is_heap_env_vec {
                                    let drop_fn = self.emit_vec_elem_closure_env_drop_fn();
                                    self.track_vec_of_aggs_var(slot_ptr, elem_ty, drop_fn);
                                } else if is_tensor_elem {
                                    self.track_vec_of_tensors_var(slot_ptr);
                                } else if let Some(map_drop) = map_elem_drop {
                                    // `Vec[Map]` / `Vec[Set]`: elements are
                                    // opaque handles the Vec now owns (the
                                    // move-into-Vec push transferred ownership);
                                    // free each on drop (Cluster 1).
                                    self.track_vec_of_maps_var(slot_ptr, map_drop);
                                } else if let Some(agg_drop) = agg_elem_drop {
                                    // `Vec[<user struct/enum>]`: run each
                                    // element's own drop fn so enum/heap fields
                                    // the inline recursion can't see are freed
                                    // (B-2026-06-12-6 cluster 2 gap 2).
                                    self.track_vec_of_aggs_var(slot_ptr, elem_ty, agg_drop);
                                } else {
                                    self.track_vec_var(slot_ptr, Some(elem_ty));
                                }
                            }
                        }
                        // Move-aware suppression for `let outer = inner;`
                        // when `inner` is a tracked Vec / String. Both
                        // slots end up pointing at the same heap buffer;
                        // without this, both cleanups fire and double-
                        // free. Zeroing the source's `cap` makes the
                        // source's `FreeVecBuffer` a no-op (the `cap > 0`
                        // guard in `emit_scope_cleanup` skips). The new
                        // `outer` binding's track stays the unique owner.
                        // No-op for non-Identifier RHS (fresh-value
                        // constructors / call results / literals).
                        // Skipped when the RHS is an owned String/Vec
                        // param — the binding received a deep copy above
                        // and the param header must stay intact for any
                        // later consume site (see the kata-23 comment at
                        // the defensive-copy shim). Also skipped for a
                        // Vec/String for-loop borrow element (B-2026-07-05-2
                        // sibling): it got its own copy above, and the
                        // container — not this local — is the single owner
                        // that frees the aliased element buffer.
                        if !rhs_retains_own_copy {
                            self.suppress_source_vec_cleanup_for_arg(value);
                        }
                        // Sibling case for `let t: String = f"…";` — the
                        // f-string acc alloca is queued for scope cleanup
                        // and now aliases the new binding's heap buffer.
                        // See the Assign arm's matching block for the
                        // double-free rationale.
                        if let Some(acc) = staged_fstr_acc {
                            self.zero_vec_alloca_cap(acc);
                        }
                    }
                }
                // Phase 7.2 Slice DP — track value-type enum bindings
                // for scope-exit drop-function invocation. Per design
                // lock DP1, the registration site is the let-binding
                // (the alloca-creation site) rather than inside
                // `try_compile_enum_variant` (which returns a
                // `BasicValueEnum` aggregate before any alloca exists).
                // The enum name is recovered from (a) the explicit
                // type annotation, when present; (b) bare-name
                // `Variant(args)` Call → walk `enum_layouts` for the
                // enum that owns `Variant`; (c) qualified
                // `Enum.Variant(args)` Call. The `track_enum_var` helper
                // self-filters shared enums (DP3) and enums with no
                // heap-bearing payload (returns early, no IR bloat).
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    let enum_name = self.enum_name_for_binding(var_name, value, ty.as_ref());
                    if let Some(name) = enum_name {
                        if let Some(slot) = self.variables.get(var_name.as_str()) {
                            let alloca = slot.ptr;
                            self.track_enum_var(&name, alloca);
                            // B-2026-07-05-2: a for-loop heap-ENUM element moved
                            // WHOLE into a new owner (`for a in items { let x = a
                            // }`). `a` bit-copy-aliases the container's
                            // live-variant payload, so `x`'s freshly-tracked
                            // `EnumDrop` and the container's per-element drain
                            // would free the same buffer (double-free, exit 134).
                            // Deep-copy the payload in place so `x` owns it
                            // independently — the enum sibling of the struct
                            // arm's `deep_copy_for_loop_agg_element_move`. Gated
                            // to a bare for-loop-agg Identifier RHS and a
                            // non-shared enum (shared enums are RC-tracked, no
                            // value `EnumDrop` to race); a fresh
                            // constructor/call RHS already owns a unique payload
                            // and is left untouched.
                            if let ExprKind::Identifier(src) = &value.kind {
                                if self.for_loop_owned_agg_vars.contains(src.as_str()) {
                                    if let Some(layout) =
                                        self.enum_layouts.get(name.as_str()).cloned()
                                    {
                                        if !layout.is_shared {
                                            self.deep_copy_enum_heap_payload_in_place(
                                                &name, alloca, &layout,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        // #9: `let g = f` enum move — the aggregate is copied
                        // into `g`'s slot (both slots alias the same heap
                        // payload), and `g`'s freshly-tracked `EnumDrop` above is
                        // the new owner. Suppress the SOURCE `f`'s `EnumDrop`
                        // (cap-zero, via the enum arm in
                        // `suppress_source_vec_cleanup_for_arg_ex`) so it no-ops;
                        // otherwise both free the same buffer (double-free). The
                        // struct-centric move-suppression below is gated on the
                        // destination carrying a `var_type_names` entry, which an
                        // unannotated enum `let g = f` need not have — so the
                        // enum case is suppressed here, at its own track site.
                        // No-op for a fresh-value RHS (constructor / call result).
                        //
                        // B-2026-06-14-31: a SHARED enum (`shared enum Expr`)
                        // is excluded. `track_enum_var` above no-op'd for it
                        // (DP3) — there is no value-`EnumDrop` to cap-zero — so
                        // the only thing the suppressor does here is emit a
                        // spurious aliasing-acquire `emit_refcount_inc` on the
                        // source (`apply_shared_transfer`), on TOP of the inc
                        // the shared-info let path already emitted for the
                        // destination. That double-inc pins the box at rc=1
                        // after both `RcDec`s run, leaking the whole tree on a
                        // `let t2 = t1` move-out that is later consumed (the
                        // Linux-CI LSan gate; silent under mac ASAN). Shared
                        // STRUCT moves are already excluded the same way — the
                        // `named_aggregate` gate on the struct-centric
                        // suppressor below filters them — so this matches that
                        // discipline. A shared enum's RC accounting is complete
                        // via the destination inc + the dual scope-exit
                        // `RcDec`s (1 inc per extra owner, 1 dec per owner).
                        let dest_is_shared_enum = self
                            .enum_layouts
                            .get(name.as_str())
                            .is_some_and(|l| l.is_shared);
                        if matches!(&value.kind, ExprKind::Identifier(_)) && !dest_is_shared_enum {
                            self.suppress_source_vec_cleanup_for_arg(value);
                        }
                        // #19: an ENUM field moved OUT of an owned (entry-copied
                        // or local) struct (`let tk = t.token` — the bootstrap
                        // lexer's `render()`). The moved-out binding `tk` (tracked
                        // just above) now owns the enum buffer, so cap-zero the
                        // source enum field in the owning struct's slot; without it
                        // BOTH the owning struct's drop and `tk`'s drop free the
                        // same buffer (double-free). Mirrors the Vec/String
                        // field-move-out suppression below (#17 gap 2). Skip a
                        // caller-retains `owned_struct_params` source — it has no
                        // struct drop to suppress (the deep-copy path owns that).
                        if let ExprKind::FieldAccess { object, .. } = &value.kind {
                            let obj_name = match &object.kind {
                                ExprKind::Identifier(o) => Some(o.as_str()),
                                ExprKind::SelfValue => Some("self"),
                                _ => None,
                            };
                            if let Some(obj) = obj_name {
                                if !self.owned_struct_params.contains(obj) {
                                    self.suppress_struct_field_move_into_literal(value);
                                }
                            }
                            // #27 — `let tk = h.ps.0.tok`: the enum field's OBJECT
                            // is a deeper place (`h.ps.0`, a tuple element), which
                            // the Identifier/`self`-gated suppressor above can't
                            // reach. Cap-zero the enum payload via the place-chain
                            // machinery so the owning struct's drop skips it.
                            self.suppress_place_field_enum_move_source(value);
                        }
                        // #21 — `let x = h.pe.0`: an enum moved out of a struct's
                        // TUPLE field. Cap-zero that tuple element in the source so
                        // the owning struct's `NestedTuple` drop skips the buffer
                        // `x` (tracked just above) now owns. Tuple-index peer of the
                        // #19 FieldAccess move-out above (P4).
                        self.suppress_tuple_index_move_source(value);
                    }
                }
                // B-2026-06-11-4 part a: a let-bound TUPLE with heap fields
                // (`let t = (i, f"x")`) has no type name, so `track_struct_var`
                // (named structs), the Vec/String/Map tracks, and `track_enum_var`
                // above all skip it — its String/Vec field had no scope-exit drop
                // and leaked. Register the anonymous-aggregate drop. Guard: the
                // slot holds a heap-bearing struct VALUE that is NOT the Vec
                // struct (those are String/Vec) and whose binding carries NO type
                // name (named structs / enums do, and are tracked above; a tuple
                // doesn't) — i.e. exactly a tuple. `track_tuple_var` no-ops when
                // the aggregate owns no heap, and shared structs / Maps / tensors
                // hold a pointer slot (not a struct value), so they're excluded.
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    // A named struct (`struct_types`) is already `track_struct_var`'d
                    // above, and a shared struct is RC-tracked; exclude both to
                    // avoid double-free. A tuple binding carries the synthetic
                    // type name "Tuple" (in neither set), so it passes — as does
                    // any other anonymous heap aggregate.
                    let named_aggregate =
                        self.var_type_names.get(var_name.as_str()).is_some_and(|n| {
                            self.struct_types.contains_key(n.as_str())
                                || self.shared_types.contains_key(n.as_str())
                        });
                    if !named_aggregate {
                        if let Some(slot) = self.variables.get(var_name.as_str()).copied() {
                            if let BasicTypeEnum::StructType(agg_ty) = slot.ty {
                                if agg_ty != self.vec_struct_type() {
                                    // Tuple-store slice (B-2026-06-22-2): a heap-env
                                    // closure stored in a tuple element
                                    // (`let t = (make(k), ..)` / `(f, ..)`) is
                                    // RC-dropped per-instance via a `FreeClosureEnv`
                                    // on that element — separate from the type-driven
                                    // tuple drop, which leaves `Fn` elements alone (a
                                    // same-frame stack-env closure must not be freed).
                                    // A no-op unless `value` is a tuple literal with a
                                    // heap-env element.
                                    self.register_tuple_literal_heap_env_elem_drops(
                                        value, slot.ptr, agg_ty,
                                    );
                                    // Container-escape caller-adopt: `let r = build(k)`
                                    // where `build` returns a closure-owning tuple —
                                    // register a per-element `FreeClosureEnv` on `r`
                                    // (no inc; the callee moved the boxes out). No-op
                                    // unless `value` is such a call.
                                    self.register_container_call_heap_env_elem_drops(
                                        value, var_name,
                                    );
                                    // Owner copy `let s = t` (`t` a tuple owner):
                                    // COPY semantics — inc the shared RC env per
                                    // owned element + register `s`'s own
                                    // per-element `FreeClosureEnv` (`t` stays live).
                                    // No-op unless `value` is an identifier naming
                                    // a tuple owner.
                                    self.register_owner_copy_container_heap_env_elem_drops(
                                        value, var_name,
                                    );
                                    if self.aggregate_has_heap_field(agg_ty) {
                                        // Proven LLVM-type path: a tuple whose heap
                                        // is a directly-visible Vec/String field
                                        // (or nested Vec aggregate). The aggregate
                                        // drop walk frees it.
                                        self.track_tuple_var(slot.ptr, agg_ty);
                                        // `let u = t` tuple-to-tuple move: both slots
                                        // alias the same buffers; zero the source's
                                        // field caps so its drop no-ops and `u` owns
                                        // (no-op for a fresh tuple-literal RHS, which
                                        // isn't an Identifier).
                                        self.suppress_source_vec_cleanup_for_arg(value);
                                    } else if let Some(elem_tes) =
                                        self.tuple_binding_elem_tes(ty.as_ref(), value)
                                    {
                                        // #23/#24 — a tuple whose only heap is an
                                        // enum / Map / Set leaf is INVISIBLE to
                                        // `aggregate_has_heap_field` (all-i64 payload
                                        // words, no `vec_struct` field), so the LLVM
                                        // path above registers no drop. The leaf then
                                        // leaked — or, once the source binding's free
                                        // is suppressed (Part B for a Map element) and
                                        // the owning struct's #21 NestedTuple drop
                                        // exists, double-freed. Register the
                                        // `TypeExpr`-driven tuple drop
                                        // (`emit_tuple_elem_drops`, which frees
                                        // enum/Map/struct leaves) keyed on element
                                        // types from the annotation or the RHS literal.
                                        if elem_tes.iter().any(|e| self.type_expr_has_drop_heap(e))
                                        {
                                            if let Some(drop_fn) =
                                                self.synthesize_tuple_drop_fn_te(agg_ty, &elem_tes)
                                            {
                                                if let Some(frame) =
                                                    self.scope_cleanup_actions.last_mut()
                                                {
                                                    frame.push(
                                                        super::state::CleanupAction::StructDrop {
                                                            struct_alloca: slot.ptr,
                                                            drop_fn,
                                                        },
                                                    );
                                                }
                                            }
                                            self.suppress_source_vec_cleanup_for_arg(value);
                                        }
                                    }
                                }
                            } else if let BasicTypeEnum::ArrayType(arr_ty) = slot.ty {
                                // Array-store slice (B-2026-06-22-2): a heap-env
                                // closure stored in a fixed-size array element
                                // (`let a: Array[Fn,N] = [make(k), ..]` / `[f, ..]`)
                                // is RC-dropped per-instance via a `FreeClosureEnv` on
                                // that element GEP. There is no type-driven drop for a
                                // `Fn`-element array (a `{ptr,ptr}` element reads as
                                // POD), so without this the env would leak. A no-op
                                // unless `value` is an array literal with a heap-env
                                // element. The array twin of the tuple branch above.
                                self.register_array_literal_heap_env_elem_drops(
                                    value, slot.ptr, arr_ty,
                                );
                                // Container-escape caller-adopt: `let r = build(k)`
                                // where `build` returns a closure-owning array.
                                self.register_container_call_heap_env_elem_drops(value, var_name);
                                // Owner copy `let s = a` (`a` an array owner):
                                // COPY semantics — inc the shared RC env per owned
                                // element + register `s`'s own per-element
                                // `FreeClosureEnv` (`a` stays live). No-op unless
                                // `value` is an identifier naming an array owner.
                                self.register_owner_copy_container_heap_env_elem_drops(
                                    value, var_name,
                                );
                            }
                        }
                    }
                }
                // B-2026-06-11-6: record a tuple binding's per-element type
                // names so a struct-field access through a tuple element
                // (`t.1.name`) resolves the element's struct type in
                // `type_name_of_expr` (structural — span-keyed lookup can't
                // distinguish `t` / `t.1` / `t.1.name`). Source: the type
                // annotation if present, else the RHS tuple literal's elements.
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    let elem_names: Option<Vec<Option<String>>> = ty
                        .as_ref()
                        .and_then(|te| match &te.kind {
                            TypeKind::Tuple(elems) => Some(
                                elems
                                    .iter()
                                    .map(|e| match &e.kind {
                                        TypeKind::Path(p) => p.segments.first().cloned(),
                                        _ => None,
                                    })
                                    .collect(),
                            ),
                            _ => None,
                        })
                        .or_else(|| match &value.kind {
                            ExprKind::Tuple(elems) => {
                                Some(elems.iter().map(|e| self.type_name_of(e)).collect())
                            }
                            _ => None,
                        });
                    if let Some(names) = elem_names {
                        self.tuple_var_elem_type_names
                            .insert(var_name.clone(), names);
                    }
                }
                // B-2026-06-10-6: a let-bound `Option[String]` /
                // `Option[Vec[_]]` whose payload is never destructured leaks
                // its inline heap — the type-erased `Option` `track_enum_var`
                // above is a no-op for it (its drop switch can't free a
                // payload that's a buffer for `Option[String]` but a scalar
                // for `Option[i64]`). Register a concrete-typed scope-exit
                // free keyed on the RHS's instantiated type. Gated to
                // Call-shaped RHS (variant constructors `Some(..)` + user-fn
                // returns) — exactly the forms that leak today. Method-call
                // results are deliberately excluded: `pop` is already freed
                // via the binding's Vec machinery (a second free would
                // double-free), and `get`/`first`/`last` return a borrow
                // (`Option[ref T]`, which `option_inline_payload_elem`
                // rejects anyway).
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    // B-2026-07-09-13: `let g = coll.get(k)` (also `.first()` /
                    // `.last()`) where the RHS is a borrow-returning stdlib
                    // collection accessor — `Map`/`SortedMap` `.get` yields an
                    // `Option[V]` whose payload ALIASES the bucket's stored
                    // value, and `Vec`/`Slice`/`Array`/`VecDeque`
                    // `.get`/`.first`/`.last` yield `Option[ref T]` aliasing
                    // element storage. `g`'s own scope-exit drop is (correctly)
                    // suppressed by the borrow model, but without recording the
                    // binding the alias property is lost at a later `match g` /
                    // `if let Some(v) = g`, so an arm that MOVES or drops `v`
                    // frees the aliased buffer a second time — double-freeing
                    // against the collection's element drop. Record the binding
                    // → its `Option[..]` type so `scrutinee_is_borrowed_binding`
                    // re-admits it into the borrow protection, matching what the
                    // DIRECT `match coll.get(k)` scrutinee already gets via
                    // `scrutinee_is_borrow_call`: a `Map` payload clones on
                    // escape (`borrow_get_payload_clone_te`), a `ref`-typed
                    // `Vec` payload self-gates to alias-only. Receiver-gated to
                    // the known stdlib collections so a user type's owned-return
                    // `.get` is untouched.
                    if let ExprKind::MethodCall { object, method, .. } = &value.kind {
                        if matches!(method.as_str(), "get" | "first" | "last")
                            && matches!(
                                self.inferred_receiver_type(object).as_deref(),
                                Some("Map")
                                    | Some("SortedMap")
                                    | Some("Vec")
                                    | Some("Slice")
                                    | Some("Array")
                                    | Some("VecDeque")
                            )
                        {
                            if let Some(te) = self
                                .enum_inst_type_exprs
                                .get(&(value.span.offset, value.span.length))
                                .cloned()
                            {
                                self.borrow_accessor_let_payload
                                    .insert(var_name.clone(), te);
                            }
                        }
                    }
                    // Call-shaped RHS (constructors `Some(..)`/`Ok(..)` +
                    // user-fn returns) OR a non-Call RHS that still yields a
                    // FRESH-owned enum — `let x = if c { Some(a) } else
                    // { None };` and match/block tails of the same
                    // (B-2026-06-10-6's non-Call follow-on). `rhs_is_fresh_inline_enum`
                    // excludes moves/aliases of existing bindings and borrows
                    // (which would double-free), so the broadening only adds
                    // provably-fresh shapes; the detectors still reject
                    // non-heap / borrow payloads.
                    if matches!(value.kind, ExprKind::Call { .. })
                        || self.rhs_is_fresh_inline_enum(value)
                    {
                        let opt_te = self
                            .enum_inst_type_exprs
                            .get(&(value.span.offset, value.span.length))
                            .cloned()
                            .or_else(|| ty.clone())
                            .or_else(|| self.untyped_let_boxed_enum_te(value));
                        if let Some(te) = opt_te {
                            if let Some(slot) = self.variables.get(var_name.as_str()).copied() {
                                // `te` is an `Option[Vec/String]`,
                                // `Option[Map/Set]`, or a `Result[..,..]`;
                                // each registrar no-ops for the other shapes.
                                // Same Call-gated leaking forms (constructors
                                // + user-fn returns).
                                self.track_inline_option_payload_var(var_name, slot.ptr, &te);
                                self.track_inline_result_payload_var(var_name, slot.ptr, &te);
                                self.track_inline_option_map_payload_var(var_name, slot.ptr, &te);
                            }
                        }
                    }
                }
                // Slice γ (2026-05-14): track value-type struct bindings
                // for scope-exit drop-fn invocation. Mirrors the enum
                // tracking above. The drop fn frees per-field heap
                // content (Vec/String data buffers, Map/Set handles).
                // `track_struct_var` self-filters shared structs (those
                // use RC) and structs with no heap-owning fields (the
                // synthesis returns `None`, no IR bloat). Struct name
                // is recovered from `var_type_names` populated by the
                // explicit-annotation / struct-literal / fresh-call
                // paths in `bind_pattern` / `compile_struct_init`.
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    if let Some(struct_name) = self.var_type_names.get(var_name.as_str()).cloned() {
                        // Phase 7 user-`impl Drop` dispatch Prereq.3:
                        // when the struct's type has a validated user
                        // Drop impl, route cleanup through the
                        // `karac_drop_<Type>` wrapper (which invokes
                        // the user body then defers to the existing
                        // field-cleanup synthesiser). The wrapper and
                        // the StructDrop action both target the same
                        // `__karac_drop_struct_<Type>` field walk, so
                        // we register exactly one of the two to avoid
                        // a double-cleanup of fields.
                        //
                        // Stdlib types (TcpListener, TcpStream) with
                        // user Drop register here even though they're
                        // NOT in `struct_types` — that map is filled
                        // by `declare_structs` walking `program.items`,
                        // which doesn't include stdlib items. The
                        // user-drop wrapper for them is hand-rolled
                        // by `emit_hardcoded_stdlib_drop_bodies`
                        // (slice 9d). For user types both paths
                        // coexist; `struct_types` containing the type
                        // is the existing trigger for `track_struct_var`,
                        // and `drop_method_keys` is the trigger for
                        // `track_user_drop_var`.
                        let has_user_drop = self
                            .program_snapshot
                            .as_deref()
                            .map(|p| p.drop_method_keys.contains_key(&struct_name))
                            .unwrap_or(false);
                        // Move-suppression: when the RHS is an
                        // Identifier, the source binding's value has
                        // been moved into the destination. The source
                        // is logically dead from this point forward;
                        // firing its UserDrop at scope exit would
                        // double-drop the same logical value
                        // (double-close fds, double-call user Drop
                        // body). Suppress the source's UserDrop action
                        // BEFORE registering the destination's so the
                        // search doesn't find the freshly-pushed
                        // duplicate when the source name happens to
                        // collide with the destination name in
                        // shadowing patterns. Only applies to the
                        // user-Drop path — the existing StructDrop /
                        // FreeVecBuffer suppression is a broader
                        // concern tracked separately in
                        // phase-7-codegen.md.
                        if let ExprKind::Identifier(source_name) = &value.kind {
                            if has_user_drop {
                                self.suppress_user_drop_for_var(source_name);
                            } else {
                                // StructDrop move-suppression: `let g = f;`
                                // where `f` is a tracked non-shared struct
                                // (e.g. an HTTP `Response` from `Ok(resp)`,
                                // now StructDrop-tracked per phase-8 line 39)
                                // copies the aggregate into `g`'s slot — both
                                // slots alias the same heap buffers + i64
                                // side-table handle. Zero the source's
                                // heap-field state (Vec/String caps + the
                                // HTTP handle) so the source's StructDrop
                                // no-ops; `g`'s freshly-registered StructDrop
                                // below becomes the sole owner. Without this,
                                // both drop fns free the same buffers (the
                                // double-free that hung the move-out E2E).
                                // No-op for non-Identifier (fresh-value) RHS.
                                //
                                // COPY site (`let g = f;`). Skip the shared
                                // transfer-inc IFF this binding already took a
                                // receive-inc — i.e. `shared_info` is Some (a
                                // bare `shared struct`, inc'd at the
                                // `shared_info` block above). Emitting the
                                // transfer-inc too would double-count → whole-
                                // chain leak (tail-cursor builder, kata #19).
                                // When `shared_info` is None — an
                                // `Option[shared T]` binding (`let mut fast =
                                // head;`), which gets NO receive-inc there —
                                // the transfer-inc is the binding's SOLE inc
                                // and must fire, else the chain is under-
                                // counted → over-dec / double-free. The
                                // Vec/String + non-shared-StructDrop handle
                                // zeroing runs regardless.
                                self.suppress_source_vec_cleanup_for_arg_ex(
                                    value,
                                    shared_info.is_none(),
                                );
                            }
                        }
                        // #27 (B-2026-06-14-8) — `let inr = h.ps.0`: a heap-bearing
                        // STRUCT moved OUT of a tuple element. `inr` is tracked
                        // (`track_struct_var` below, registered as its struct type
                        // by the typechecker/lowering annotation) and now owns the
                        // buffers; cap-zero the SOURCE tuple element in the owning
                        // struct's slot so its `NestedTuple` drop skips them — else
                        // both `inr`'s `__karac_drop_struct_<Inner>` and `h`'s
                        // `__karac_drop_struct_<Hs>` free the same buffer
                        // (double-free). The struct sibling of the enum-path
                        // `suppress_tuple_index_move_source` call (#21 P4);
                        // `zero_tuple_elem_cap_at` routes a struct element through
                        // `zero_struct_move_caps` (recurses into the enum/Vec leaf).
                        if matches!(&value.kind, ExprKind::TupleIndex { .. }) {
                            self.suppress_tuple_index_move_source(value);
                        }
                        if let Some(slot) = self.variables.get(var_name.as_str()) {
                            let alloca = slot.ptr;
                            // A shared struct's user `impl Drop` is fired by the
                            // RC path (`track_rc_var` → `emit_rc_dec` →
                            // `__karac_rc_drop_<T>`, which calls the body at
                            // refcount→0), NOT the value-type `UserDrop` drain.
                            // Registering `track_user_drop_var` here too would
                            // (a) fire the body twice and (b) pass `alloca` —
                            // the slot holding the heap *pointer* — to
                            // `<T>.drop`, so `self.<field>` would dereference a
                            // pointer-to-pointer and crash. Gate it out for
                            // shared structs. (phase-7 L938)
                            if has_user_drop && !self.shared_types.contains_key(&struct_name) {
                                self.track_user_drop_var(&struct_name, var_name, alloca);
                            } else if self.struct_types.contains_key(&struct_name) {
                                self.track_struct_var(&struct_name, alloca);
                            }
                            // B-2026-07-04-17: `x`'s heap aliases a heap-owning
                            // for-loop struct ELEMENT the container's per-element
                            // drop frees. Deep-copy the aliasing field(s) of `x`
                            // in place so `x`'s struct-drop (just registered) and
                            // the container's drain free independent buffers.
                            // Two shapes:
                            //   `let x = a`            → whole-struct move of the
                            //                            element: copy every field.
                            //   `let w = A { s: a.s }` → a fresh literal whose
                            //                            field(s) move heap OUT of
                            //                            the element: copy ONLY the
                            //                            element-sourced fields (a
                            //                            sibling field from a fresh
                            //                            value must NOT be copied —
                            //                            that would leak it).
                            if self.struct_types.contains_key(&struct_name)
                                && !self.shared_types.contains_key(&struct_name)
                            {
                                self.deep_copy_for_loop_agg_element_move(
                                    value,
                                    alloca,
                                    &struct_name,
                                );
                            }
                            // Store-in-struct slice (B-2026-06-22-2): a fresh
                            // heap-env closure stored in a struct field
                            // (`H { f: make(..) }`) is RC-dropped per-instance via
                            // a `FreeClosureEnv` on that field — separate from the
                            // type-driven struct drop, which leaves `Fn` fields
                            // alone (a same-frame stack-env closure must not be
                            // RC-freed).
                            self.register_struct_literal_heap_env_field_drops(
                                value,
                                &struct_name,
                                alloca,
                                var_name,
                            );
                            // Aggregate-escape slice (B-2026-06-22-2): `let r =
                            // build(k)` where `build` returns a heap-env-owning
                            // struct — register an instance `FreeClosureEnv` on each
                            // owned field of `r` (the callee moved the env boxes out
                            // at the same refcount; `r` is now their sole RC-owner).
                            self.register_aggregate_call_heap_env_field_drops(
                                value,
                                &struct_name,
                                alloca,
                                var_name,
                            );
                            // Owner-copy slice (B-2026-06-22-2): `let s = a` where
                            // `a` is a heap-env struct owner — INC the shared RC env
                            // of each owned field and register `s`'s own instance
                            // `FreeClosureEnv` (COPY semantics; `a` stays live). No-op
                            // unless the RHS is an identifier naming a struct owner.
                            self.register_owner_copy_struct_heap_env_field_drops(
                                value, alloca, var_name,
                            );
                        }
                    }
                }
                // Track Map/Set variables when the RHS is a fresh-handle-producing
                // method call (`clone`, `union`, `intersection`, `difference`).
                // `Map.new()` / `Set.new()` / map-literal RHS shapes already track
                // via their early-return paths above; `let n = m;` (move) bypasses
                // this since it's an Identifier RHS, not a MethodCall, so the
                // source's existing track stays the unique cleanup owner.
                //
                // ALSO track when the RHS is a `Call` returning a Map/Set BY VALUE
                // (`let m2 = make_map()`). An owned by-value return transfers the
                // handle to this binding — the callee suppressed its own
                // `FreeMapHandle` on the move-out `return m;`, so the binding is now
                // the unique freer. Without this the handle leaked (Linux LSan;
                // silent on macOS). EXCLUDE borrow-returning calls
                // (`fn_ref_return_inner` — `ref Map` accessors): those alias the
                // container's storage and must not be double-freed. `Map.new()` and
                // map literals never reach here (early returns above); a place
                // source (`let mm = s.m`) is a FieldAccess/Index, not a `Call`, so
                // it stays a caller-retains alias as before (#28 / B-2026-06-14-9).
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    let fresh_handle = matches!(
                        &value.kind,
                        ExprKind::MethodCall { method, .. }
                            if matches!(
                                method.as_str(),
                                "clone" | "union" | "intersection" | "difference"
                            )
                    ) || (matches!(&value.kind, ExprKind::Call { .. })
                        && !self.is_borrow_returning_call_expr(value));
                    if fresh_handle
                        && (self.map_key_types.contains_key(var_name.as_str())
                            || self.set_elem_types.contains_key(var_name.as_str()))
                    {
                        if let Some(slot) = self.variables.get(var_name.as_str()).copied() {
                            // `key_is_vec` reads from `map_key_types` for Map
                            // bindings or `set_elem_types` for Set bindings
                            // (Set lowers to Map[T, ()] with the elem type
                            // as the "key"). `val_is_vec` reads only from
                            // `map_val_types` — Sets have val_size = 0 so
                            // their val_is_vec is always false.
                            let key_is_vec = self
                                .map_key_types
                                .get(var_name.as_str())
                                .or_else(|| self.set_elem_types.get(var_name.as_str()))
                                .copied()
                                .is_some_and(|t| self.llvm_ty_is_vec_struct(t));
                            let val_is_vec = self
                                .map_val_types
                                .get(var_name.as_str())
                                .copied()
                                .is_some_and(|t| self.llvm_ty_is_vec_struct(t));
                            let val_shared_heap =
                                self.map_val_shared_heap_type_for(var_name.as_str());
                            let key_shared_heap =
                                self.map_key_shared_heap_type_for(var_name.as_str());
                            // Slice 3r: per-value drop fn for non-overlay heap
                            // values; owns the whole value side when present.
                            let val_drop_fn = self
                                .var_elem_type_exprs
                                .get(var_name.as_str())
                                .cloned()
                                .and_then(|vte| self.map_val_drop_fn_for_type_expr(&vte));
                            let (val_is_vec, val_shared_heap) = if val_drop_fn.is_some() {
                                (false, None)
                            } else {
                                (val_is_vec, val_shared_heap)
                            };
                            self.track_map_var_with_val_drop(
                                slot.ptr,
                                key_is_vec,
                                val_is_vec,
                                val_shared_heap,
                                key_shared_heap,
                                val_drop_fn,
                            );
                        }
                    }
                }
                // Channel-end move-rebind: `let keep = rx;` where the RHS is a
                // bare Identifier naming a `Sender`/`Receiver` binding. The
                // destructure (`let (tx, rx) = Channel.new()`) queued a
                // `DropChannelEnd` for `rx`, and `bind_pattern` just queued a
                // SECOND one for `keep` (`track_channel_var`, keyed on keep's
                // alloca, fired because `keep`'s surface type is also a channel
                // end). Both decrement the same channel refcount at scope exit —
                // an over-drop that frees the `KaracChannel` early (the
                // recv-out-slot race: a double-free / heap-UAF under ASAN). This
                // is the let-rebind sibling of the move-out-on-return fix in the
                // `ExprKind::Return` arm and `suppress_cleanup_for_tail_return`.
                //
                // Channel ends have no in-slot `cap = 0` sentinel like
                // Vec/String, but `karac_runtime_channel_drop_*` are null-handle
                // no-ops, so we synthesize one: `neutralize_moved_channel_end_slot`
                // KEEPS `rx`'s queued `DropChannelEnd` and nulls `rx`'s slot at
                // the move site, so the source drop no-ops at runtime — and only
                // on the path that executed the move. A plain compile-time
                // retraction (`suppress_channel_drop_for_var`, used at the
                // terminal `return`/`spawn` move sites) would over-suppress a
                // branch-buried rebind: `if c { let keep = rx } else { rx.recv() }`
                // would leak the channel on the `else` arm. Gated to a bare
                // Identifier RHS — a genuine move; `let tx2 = tx.clone()` is a
                // MethodCall that mints a fresh refcount and MUST keep its own
                // drop, so it is excluded. The destination surface-type check
                // (`pattern_binding_types`) mirrors `bind_pattern`'s own
                // registration gate, so source-neutralization and
                // dest-registration are symmetric.
                if let (PatternKind::Binding(_), ExprKind::Identifier(rhs_name)) =
                    (&pattern.kind, &value.kind)
                {
                    let key = (pattern.span.offset, pattern.span.length);
                    if matches!(
                        self.pattern_binding_types.get(&key).map(String::as_str),
                        Some("Sender") | Some("Receiver")
                    ) {
                        self.neutralize_moved_channel_end_slot(rhs_name);
                    }
                }
                // Slice c-repl.B.5.1: REPL value-snapshot capture site.
                // After the binding's slot is alloca'd + populated, copy
                // the bound value into the cell-spanning
                // `__karac_repl_snapshot_<name>` global so subsequent
                // cells can replay the value without re-evaluating the
                // original RHS. No-op when the binding name is not in
                // `snapshot_capture` (every non-REPL build, plus REPL
                // cells whose binding doesn't qualify for snapshotting
                // — non-primitive type, destructuring pattern, etc.).
                self.try_emit_snapshot_capture(pattern);
                // Binary-search midpoint BCE: under a dominating strict
                // `while lo < hi`, a `let mid = lo + (hi - lo) / 2` binding
                // emits `assume(lo <= mid < hi)` so LLVM folds the
                // `nums[mid]` bounds check (control_flow_bce.rs § midpoint).
                self.try_emit_binsearch_midpoint_assumes(pattern, value);
                Ok(())
                // (`Set.new()` and `Map.new()` register their own
                // `FreeMapHandle` cleanup inside `compile_set_new_stmt` /
                // `compile_map_new_stmt` — those are early returns so
                // they don't reach this fallback.)
            }
            StmtKind::Expr(expr) => {
                // General owned-temp tracking, slice 1 (see
                // `docs/spikes/general-owned-temp-tracking.md`): a value
                // produced in statement position by a fresh-owned-yielding
                // call (`make_vec();`, `s.to_upper();`) is a discarded
                // temporary with no binding to drop it, so its heap buffer
                // would leak. Route it through the owned-temp chokepoint
                // inside a one-shot scope frame so it drops at the `;`
                // (design.md § Temporary Lifetime Rules — statement-position
                // temporaries drop at the `;`). Gated to Call/MethodCall so a
                // discarded *place* expression is never double-freed against
                // its binding's own cleanup. `materialize_owned_temp` handles
                // Vec/String (LLVM-type-detectable) plus Map/Set handles and
                // RC boxes via the `owned_temp_drops` hint table keyed on the
                // expression span. When the gate is false the arm behaves
                // exactly as before.
                // Slice 5 extends the gate through single-tail block wrappers:
                // `{ make() }` in statement position discards the block's tail
                // temp (the block returns it; its frame drops only the block-
                // local lets), so route that tail through the chokepoint too.
                // `compile_expr(expr)` returns the block's tail value, and the
                // hint table is keyed on the *tail* expr's span (not the
                // block's), so element/Map/RC types resolve correctly. Direct
                // `make();` is the degenerate tail == expr case — unchanged.
                let tail = Self::discarded_owned_temp_tail(expr);
                if tail.is_some() {
                    self.scope_cleanup_actions.push(Vec::new());
                }
                let val = self.compile_expr(expr)?;
                // Phase-8 line 39 follow-up — `c.request(url).header(...);`
                // discards a live RequestBuilder temporary; free its
                // abandoned HTTP_BUILDERS handle (no-op for non-builder /
                // already-sent chains).
                self.free_discarded_request_builder_temp(expr, val);
                if let Some(tail) = tail {
                    // B-2026-06-10-6: a discarded inline-`Option` temp
                    // (`v.pop();`, `make_opt();`) leaks its `String`/`Vec`
                    // payload — the erased Option drop switch can't free it
                    // and there's no binding to. Free it here, but NOT when
                    // the producer returns a borrow (`get`/`first`/`last`/
                    // `Map.get` alias the container's storage — freeing would
                    // corrupt it). Falls back to the generic owned-temp
                    // chokepoint (Vec/String/Map/RC) for everything else.
                    // Same treatment for a discarded inline-`Result` temp
                    // (`Result` follow-on); the two registrars are mutually
                    // exclusive on the producer's instantiated type.
                    let not_borrow = !self.scrutinee_is_borrow_call(tail);
                    let handled_option =
                        not_borrow && self.try_track_discarded_inline_option(tail, val);
                    let handled_result = !handled_option
                        && not_borrow
                        && self.try_track_discarded_inline_result(tail, val);
                    let handled_option_map = !handled_option
                        && !handled_result
                        && not_borrow
                        && self.try_track_discarded_inline_option_map(tail, val);
                    // Slice 3r: a discarded BOXED-payload Option temp
                    // (`m.insert(k, v2);` displacing a struct value,
                    // `m.remove(k);` moving one out) owns both the box and
                    // the payload's interior heap — the inline trackers
                    // above all decline wide payloads.
                    let handled_boxed_option = !handled_option
                        && !handled_result
                        && !handled_option_map
                        && not_borrow
                        && self.try_track_discarded_boxed_option(tail, val);
                    // B-2026-07-01-7 (discard position): `make();` where
                    // `make() -> Guard`/`-> Sig` with a user Drop — the
                    // discarded temp is caller-owned and its body must fire
                    // (both surfaces were silent). Complementary to the
                    // heap-content trackers above; its registration is
                    // type-gated internally.
                    if not_borrow {
                        self.try_track_discarded_user_drop_temp(tail, val);
                    }
                    if !handled_option
                        && !handled_result
                        && !handled_option_map
                        && !handled_boxed_option
                    {
                        self.materialize_owned_temp(val, (tail.span.offset, tail.span.length));
                    }
                    self.drain_top_frame_with_emit();
                }
                Ok(())
            }
            StmtKind::Assign { target, value } => {
                // Phase-B2 link-store fast path: `<bare>.link =
                // Some(<fresh>)` (or `= None`) on a b2 cluster target
                // collapses to a single pointer store into the niche
                // slot — no Some-ctor payload inc, no field-store
                // retain/release. The analysis guarantees the target's
                // old link is structurally None (displacement-free
                // shapes only), so there is nothing to release.
                // Intercepted BEFORE the generic value compile so the
                // `Some(...)` constructor (which incs shared payloads)
                // never runs. Falls through on any shape mismatch.
                if self.try_emit_b2_link_store(target, value)? {
                    return Ok(());
                }
                // `*m.entry(k).or_insert(d) = v` — store through the entry slot
                // pointer (compiled once, before the RHS). Mirrors the
                // interpreter's MapSlotRef write for A/B parity. Scalar values
                // store cleanly; for a heap value type the prior slot contents
                // are not dropped here (this assign-through-entry shape is rare
                // — counters use `+=`, per-key Vecs use `.push`).
                if let ExprKind::Unary {
                    op: crate::ast::UnaryOp::Deref,
                    operand,
                } = &target.kind
                {
                    if self.entry_chain_or_insert_map_name(operand).is_some() {
                        let slot_ptr = self.compile_expr(operand)?.into_pointer_value();
                        let v = self.compile_expr(value)?;
                        self.builder.build_store(slot_ptr, v).unwrap();
                        return Ok(());
                    }
                }
                // `*r = v` / `r = v` where `r` is a let-bound entry slot ref:
                // store through the slot pointer (two-step idiom, parity with
                // the interpreter's MapSlotRef write).
                if let Some(name) = self.entry_slot_ref_target(target) {
                    let (slot_ptr, _val_ty) = self.entry_slot_ref_ptr(&name)?;
                    let v = self.compile_expr(value)?;
                    self.builder.build_store(slot_ptr, v).unwrap();
                    return Ok(());
                }
                // SoA reassignment from a layout-returning call — the carried-
                // grid double-buffer move `grid = substep(grid, …)` (the host's
                // per-frame loop). The let arm has `compile_soa_let_from_call`;
                // this is its assignment sibling. Without it the call returns the
                // AoS `{ptr,len,cap}` struct (no backward mono fires, since
                // `pending_return_layout` is parked only by the let path), and
                // the 3-field value is stored into the existing 4-field SoA slot
                // → garbage group pointers → SIGSEGV on the next index read.
                if let ExprKind::Identifier(name) = &target.kind {
                    if let Some(soa) = self.active_soa_layout(name) {
                        if self.let_rhs_calls_layout_returning_fn(value) {
                            let name = name.clone();
                            return self.compile_soa_assign_from_call(&name, &soa, value);
                        }
                    }
                }
                // Mirror the let-site convention: when the RHS is a
                // `StructLiteral` (`emit_rc_alloc` returns rc=1) or a
                // `Call` / `MethodCall` (callee transfers +1 via the
                // return value — see the let-site comment), the value
                // already carries a fresh ref. Skip the receive-side
                // `rc_inc` to avoid doubling the refcount on
                // `x = make()` / `x = obj.make()` / shared-enum-variant
                // reassignment.
                // Same recursive tail-shape walk as the Let arm — covers
                // `x = if cond { make_a() } else { make_b() };` and the
                // `Match` / `IfLet` / `Block` equivalents.
                let rhs_is_fresh = self.rhs_yields_fresh_ref(value);
                let rhs_is_fstring = self.rhs_stages_fstr_acc(value);
                let val = self.compile_expr(value)?;
                // Owned String/Vec PARAM moved into an existing binding
                // (`work = lists;` where `lists` is a bare by-value
                // param) — same caller-frees double-free as the Let arm's
                // shim (see the kata-23 comment there): deep-copy and
                // leave the param's header intact; the move-suppression
                // below is skipped for this shape.
                let rhs_is_owned_param = matches!(
                    &value.kind,
                    ExprKind::Identifier(n) if self.owned_vecstr_params.contains(n.as_str())
                );
                let val = if rhs_is_owned_param {
                    self.maybe_defensive_copy_param_arg(value, val)
                } else {
                    val
                };
                // Consume the f-string acc staging slot once compile_expr
                // returns — even on the rare paths where the Assign arm
                // doesn't reach the transfer step below, the slot must not
                // leak into a subsequent unrelated Let / Assign whose RHS
                // is not an f-string.
                let staged_fstr_acc = if rhs_is_fstring {
                    self.last_fstr_acc.take()
                } else {
                    None
                };
                if let ExprKind::Identifier(name) = &target.kind {
                    // Assign-through for a numeric-scalar `mut ref` param
                    // (design.md § "Compound assignment on `mut ref` lvalues"
                    // :5306 — `a = a + b` on a `mut ref T` lvalue writes
                    // through to the caller's `T`). The alloca holds the borrow
                    // POINTER (the param is registered in `ref_params`), so the
                    // generic `build_store(slot.ptr, …)` below would clobber the
                    // pointer with the value instead of writing the pointee — a
                    // silent miscompile (the caller's value never changes; the
                    // interpreter, which mutates through the borrow, would then
                    // disagree with the built binary). `get_data_ptr` loads the
                    // borrow pointer, giving the caller's storage address. Scalar
                    // only: a `mut ref Vec`/`String`/struct mutates through
                    // methods / field stores that already deref via
                    // `get_data_ptr`, and routing their whole-value moves here
                    // would bypass the heap move-tracking further down.
                    if let Some(&inner_ty) = self.ref_params.get(name) {
                        if inner_ty.is_int_type() || inner_ty.is_float_type() {
                            if let Some(ptr) = self.get_data_ptr(name) {
                                let cval = self.coerce_scalar_to_type(val, inner_ty);
                                self.builder.build_store(ptr, cval).unwrap();
                                return Ok(());
                            }
                        }
                    }
                    // Slice 9: module-level `let mut BINDING = …;`
                    // identifier-LHS assignment writes directly to
                    // the LLVM global. The typechecker (slice 5)
                    // rejects writes to immutable `let`, so a hit on
                    // `try_store_module_binding` for `is_mut = false`
                    // is impossible under correct upstream behaviour;
                    // LLVM's `constant` global flag also catches the
                    // case independently as a verifier error.
                    if self.try_store_module_binding(name, val) {
                        return Ok(());
                    }
                    // Heap-env closure binding reassignment (B-2026-06-22-2):
                    // `g = make(j)` (fresh env, a MOVE) or `g = f` (binding
                    // source, the SHARED env, a COPY). RC setter rule (retain
                    // new → store → release old): save `g`'s CURRENT fat, inc
                    // the new env when the RHS is a binding copy (a fresh
                    // `make(j)` already carries its +1), store the new fat, then
                    // RC-drop the saved old env. Each env is freed EXACTLY once;
                    // on a copy the source `f` stays a live co-owner (its own
                    // scope-exit `FreeClosureEnv` balances the inc). The
                    // release-LAST order is harmless here (closure env boxes are
                    // independent — the new env is never reachable through the
                    // old) but mirrors the shared-T / `Option[shared]` setter
                    // arms below for one consistent shape. Sits ahead of those
                    // arms; a closure binding is in none of their type maps.
                    if self.heap_env_closure_vars.contains(name) {
                        if let Some(slot) = self.variables.get(name).copied() {
                            let old_fat = self
                                .builder
                                .build_load(slot.ty, slot.ptr, "clo.reassign.old")
                                .unwrap();
                            let rhs_is_binding_copy = matches!(&value.kind,
                                ExprKind::Identifier(n)
                                    if self.heap_env_closure_vars.contains(n));
                            if rhs_is_binding_copy {
                                self.emit_heap_closure_env_inc(val);
                            }
                            self.builder.build_store(slot.ptr, val).unwrap();
                            self.emit_heap_closure_env_dec(old_fat);
                            return Ok(());
                        }
                    }
                    // For shared types, the ARC setter rule: retain new →
                    // store → release old. The release MUST run last —
                    // when the new value is reachable *through* the old one
                    // (the canonical list-walk `node = node.next`), dec'ing
                    // the old ref first drops the chain and frees the new
                    // node before its inc, UAF. Same ordering bug class as
                    // the `Option[shared T]` field-store fix (25442e73);
                    // this is the variable-assign sibling.
                    if let Some(type_name) = self.var_type_names.get(name).cloned() {
                        if let Some(info) = self.shared_types.get(&type_name).cloned() {
                            if let Some(slot) = self.variables.get(name).copied() {
                                // Phase-B2 bare cursor advance
                                // (`tail = node`): non-owning alias —
                                // plain store, no inc/dec dance.
                                if self.b2_skips_counts(name) {
                                    self.builder.build_store(slot.ptr, val).unwrap();
                                    return Ok(());
                                }
                                // Save the old pointer before overwriting.
                                let old_ptr = self
                                    .builder
                                    .build_load(
                                        self.context.ptr_type(AddressSpace::default()),
                                        slot.ptr,
                                        "old_rc",
                                    )
                                    .unwrap()
                                    .into_pointer_value();
                                // rc_inc new pointer — only when the RHS
                                // is an alias of an existing tracked ref
                                // (fresh sources already carry their +1).
                                if !rhs_is_fresh {
                                    let new_ptr = val.into_pointer_value();
                                    self.emit_refcount_inc(name, info.heap_type, new_ptr);
                                }
                                self.builder.build_store(slot.ptr, val).unwrap();
                                // rc_dec old pointer, after the new ref is
                                // counted and stored.
                                self.emit_refcount_dec(name, info.heap_type, old_ptr);
                                return Ok(());
                            }
                        }
                    }
                    // `Option[shared T]` Assign — symmetric to the
                    // plain shared-T arm above, but operating on the
                    // Option struct's tag + w0 inner pointer, with the
                    // same ARC setter ordering (retain new → store →
                    // release old):
                    //   1. Save the old slot's inner pointer (null when
                    //      the old tag is None).
                    //   2. Store the new Option value.
                    //   3. If the RHS is not a fresh-ref source
                    //      (i.e., not a `Some(...)` literal or other
                    //      Call/MethodCall — those already carry a
                    //      +1 transfer; see the let-stmt comment for
                    //      the +1 handshake), branch on the new
                    //      tag; if Some, inc the new inner pointer.
                    //   4. Dec the saved old inner pointer, if non-null.
                    // Without the inc/dec pair, `mut next_a: Option[Node]`
                    // styled reassignments (recursive kata: `next_a =
                    // n.next;`) strand the old ref and over-decrement at
                    // scope exit, hanging the program on chain access.
                    // The release-LAST ordering matters when the new
                    // value is reachable through the old one (list-walk
                    // `cur = node.next` where `node` aliases `cur`'s
                    // head): dec'ing old first drops the whole chain and
                    // frees the new node before its inc — UAF. Same bug
                    // class as the field-store fix (25442e73); this is
                    // the variable-assign sibling.
                    if let Some(heap_type) = self.var_option_shared_heap.get(name.as_str()).copied()
                    {
                        if let Some(slot) = self.variables.get(name.as_str()).copied() {
                            // Phase-B2 option cursor (`cur = x.next` /
                            // `cur = None`): non-owning — plain store,
                            // no save/inc/dec.
                            if self.b2_skips_counts(name) {
                                self.builder.build_store(slot.ptr, val).unwrap();
                                return Ok(());
                            }
                            let option_ty = self.enum_layouts["Option"].llvm_type;
                            let i64_t = self.context.i64_type();
                            let ptr_ty = self.context.ptr_type(AddressSpace::default());
                            let some_tag = self
                                .enum_layouts
                                .get("Option")
                                .and_then(|l| l.tags.get("Some").copied())
                                .unwrap_or(1);
                            let some_tag_const = i64_t.const_int(some_tag, false);
                            let fn_val = self.current_fn.unwrap();
                            // ── Step 1: save the old inner pointer. The
                            //    tag/w0 loads are unconditional (loads of
                            //    our own slot are always safe); a select
                            //    collapses "old is None" and "old inner
                            //    is null" into one null sentinel so the
                            //    deferred release below needs a single
                            //    null-check branch. A None slot's w0 may
                            //    be undef — the select keeps that garbage
                            //    from ever being dereferenced.
                            let old_tag_ptr = self
                                .builder
                                .build_struct_gep(option_ty, slot.ptr, 0, "opt.assign.old.tag.p")
                                .unwrap();
                            let old_tag = self
                                .builder
                                .build_load(i64_t, old_tag_ptr, "opt.assign.old.tag")
                                .unwrap()
                                .into_int_value();
                            let old_is_some = self
                                .builder
                                .build_int_compare(
                                    IntPredicate::EQ,
                                    old_tag,
                                    some_tag_const,
                                    "opt.assign.old.is_some",
                                )
                                .unwrap();
                            let old_w0_ptr = self
                                .builder
                                .build_struct_gep(option_ty, slot.ptr, 1, "opt.assign.old.w0.p")
                                .unwrap();
                            let old_w0 = self
                                .builder
                                .build_load(i64_t, old_w0_ptr, "opt.assign.old.w0")
                                .unwrap()
                                .into_int_value();
                            let old_inner = self
                                .builder
                                .build_int_to_ptr(old_w0, ptr_ty, "opt.assign.old.inner")
                                .unwrap();
                            let old_eff = self
                                .builder
                                .build_select(
                                    old_is_some,
                                    old_inner,
                                    ptr_ty.const_null(),
                                    "opt.assign.old.eff",
                                )
                                .unwrap()
                                .into_pointer_value();
                            // ── Step 2: store the new Option value. ──
                            self.builder.build_store(slot.ptr, val).unwrap();
                            // ── Step 3: inc new inner if RHS is an
                            //           aliasing source (not a fresh
                            //           Some/None/Call/MethodCall).
                            //           Read the just-stored Option
                            //           back rather than re-extracting
                            //           from `val` so the IR stays
                            //           uniform across struct-vs-ptr
                            //           BasicValueEnum shapes.
                            if !rhs_is_fresh {
                                let new_tag_ptr = self
                                    .builder
                                    .build_struct_gep(
                                        option_ty,
                                        slot.ptr,
                                        0,
                                        "opt.assign.new.tag.p",
                                    )
                                    .unwrap();
                                let new_tag = self
                                    .builder
                                    .build_load(i64_t, new_tag_ptr, "opt.assign.new.tag")
                                    .unwrap()
                                    .into_int_value();
                                let new_is_some = self
                                    .builder
                                    .build_int_compare(
                                        IntPredicate::EQ,
                                        new_tag,
                                        some_tag_const,
                                        "opt.assign.new.is_some",
                                    )
                                    .unwrap();
                                let new_do_bb =
                                    self.context.append_basic_block(fn_val, "opt.assign.new.do");
                                let new_skip_bb = self
                                    .context
                                    .append_basic_block(fn_val, "opt.assign.new.skip");
                                self.builder
                                    .build_conditional_branch(new_is_some, new_do_bb, new_skip_bb)
                                    .unwrap();
                                self.builder.position_at_end(new_do_bb);
                                let new_w0_ptr = self
                                    .builder
                                    .build_struct_gep(option_ty, slot.ptr, 1, "opt.assign.new.w0.p")
                                    .unwrap();
                                let new_w0 = self
                                    .builder
                                    .build_load(i64_t, new_w0_ptr, "opt.assign.new.w0")
                                    .unwrap()
                                    .into_int_value();
                                let new_inner = self
                                    .builder
                                    .build_int_to_ptr(new_w0, ptr_ty, "opt.assign.new.inner")
                                    .unwrap();
                                let new_is_null = self
                                    .builder
                                    .build_is_null(new_inner, "opt.assign.new.is_null")
                                    .unwrap();
                                let new_real_do_bb = self
                                    .context
                                    .append_basic_block(fn_val, "opt.assign.new.real_do");
                                self.builder
                                    .build_conditional_branch(
                                        new_is_null,
                                        new_skip_bb,
                                        new_real_do_bb,
                                    )
                                    .unwrap();
                                self.builder.position_at_end(new_real_do_bb);
                                self.emit_refcount_inc(name, heap_type, new_inner);
                                self.builder
                                    .build_unconditional_branch(new_skip_bb)
                                    .unwrap();
                                self.builder.position_at_end(new_skip_bb);
                            }
                            // ── Step 4: release the saved old inner, now
                            //    that the new ref is counted and stored.
                            let old_is_null = self
                                .builder
                                .build_is_null(old_eff, "opt.assign.old.is_null")
                                .unwrap();
                            let old_dec_bb = self
                                .context
                                .append_basic_block(fn_val, "opt.assign.old.dec");
                            let done_bb =
                                self.context.append_basic_block(fn_val, "opt.assign.done");
                            self.builder
                                .build_conditional_branch(old_is_null, done_bb, old_dec_bb)
                                .unwrap();
                            self.builder.position_at_end(old_dec_bb);
                            self.emit_refcount_dec(name, heap_type, old_eff);
                            self.builder.build_unconditional_branch(done_bb).unwrap();
                            self.builder.position_at_end(done_bb);
                            return Ok(());
                        }
                    }
                    // Free the LHS's existing heap buffer before writing
                    // the new value, when LHS is a tracked Vec / String
                    // and the RHS won't end up aliasing it. Without the
                    // free, the OLD buffer leaks on every assignment —
                    // a loop of `s = f"…"` accumulates one leaked buffer
                    // per iteration, and a BFS frontier-swap loop
                    // (`out = next;`) leaks the entire prior frontier
                    // per outer step. The `cap > 0` guard skips static
                    // string-literal slots (cap = 0) so the inert
                    // `let mut s: String = "[";` → first assignment is
                    // free of any free; only previously heap-grown slots
                    // get reclaimed. Symmetric guard is already in the
                    // `FreeVecBuffer` cleanup walker; this is the eager-
                    // free analogue for the move-overwrite path.
                    //
                    // Triggered for RHS shapes that produce a heap buffer
                    // distinct from the LHS slot's prior buffer:
                    //   - `InterpolatedStringLit` (staged_fstr_acc set):
                    //     f-string accumulator is in a separate slot.
                    //   - `Identifier(rhs_name)` to a different tracked
                    //     Vec/String binding: source slot's buffer is
                    //     about to be moved into LHS; old LHS buffer is
                    //     orphaned. Skip when `rhs_name == name`
                    //     (self-alias `x = x` would free the buffer
                    //     we're about to point to).
                    //   - Call / MethodCall / StructLiteral
                    //     (`rhs_yields_fresh_ref` is true): the RHS
                    //     materializes a +1 transfer, distinct slot.
                    //
                    // OUTER buffer only — `emit_free_vec_buffer_if_owned`
                    // deliberately does NOT walk inner heap-owning
                    // elements (see its doc comment: a live per-element
                    // alias's own scope-exit cleanup would double-free).
                    // Without this eager outer free, kata-17's K=100k
                    // Letter-Combinations workload retains 38.5 MiB peak
                    // RSS instead of plateauing at the C/Rust working-set
                    // baseline of 1.3 MiB. Inner elements of the replaced
                    // generation still leak unless the program drains
                    // them via per-element alias bindings (kata-17's
                    // `let prefix = out[i]` pattern) — measured 2026-06-06
                    // at ~15.7 MiB for the binding-free kata-17 variant;
                    // tracked in phase-7-codegen.md § "Move-overwrite
                    // inner-element drop".
                    let lhs_is_tracked_vec = self.vec_elem_types.contains_key(name.as_str());
                    let rhs_is_self_alias = matches!(
                        &value.kind,
                        ExprKind::Identifier(rhs_name) if rhs_name == name
                    );
                    let rhs_is_moved_alias = matches!(
                        &value.kind,
                        ExprKind::Identifier(rhs_name) if rhs_name != name
                            && self.vec_elem_types.contains_key(rhs_name.as_str())
                    );
                    let trigger_eager_free = lhs_is_tracked_vec
                        && !rhs_is_self_alias
                        && (staged_fstr_acc.is_some() || rhs_is_moved_alias || rhs_is_fresh);
                    if trigger_eager_free {
                        if let Some(slot) = self.variables.get(name).copied() {
                            self.emit_free_vec_buffer_if_owned(slot.ptr);
                        }
                    }
                    if let Some(slot) = self.variables.get(name).copied() {
                        // Coerce a scalar RHS to the slot's width before
                        // storing — narrow-int arithmetic computes at i64
                        // (`compile_narrow_int_binop`), so `r = r + 1` on an
                        // `i32`/`u8` local yields an i64 that must be truncated
                        // to the `iN` slot (lossless: the op already
                        // range-checked). No-op when widths match or the value
                        // is non-scalar. Mirrors the let-binding boundary.
                        let cval = self.coerce_scalar_to_type(val, slot.ty);
                        self.builder.build_store(slot.ptr, cval).unwrap();
                    }
                    // Move-aware suppression for `outer = inner;` when
                    // the LHS is a tracked Vec / String and the RHS is
                    // an Identifier to another tracked binding. Both
                    // slots end up holding the same {ptr, len, cap};
                    // without this, both scope-exit `FreeVecBuffer`
                    // cleanups fire and double-free. The LHS's track
                    // (registered at LHS's original let-site) stays
                    // the unique cleanup owner. No-op for non-
                    // Identifier RHS — fresh-value RHS shapes can't
                    // alias an existing tracked binding.
                    if lhs_is_tracked_vec {
                        // Owned-param RHS received a deep copy above —
                        // keep the param's header intact (cap > 0) for
                        // later consume sites; see the Let arm's shim.
                        if !rhs_is_owned_param {
                            self.suppress_source_vec_cleanup_for_arg(value);
                        }
                        // Sibling case for the InterpolatedStringLit RHS
                        // shape: the f-string accumulator alloca is queued
                        // for scope-exit cleanup (see `compile_expr`'s
                        // `InterpolatedStringLit` arm at exprs.rs ~85).
                        // After `s = f"…"` the LHS's slot points at the
                        // same heap buffer as the staged acc; firing both
                        // cleanups double-frees and hangs in macOS
                        // malloc_printf. Zero the acc's `cap` so its
                        // `FreeVecBuffer` no-ops on the `cap > 0` guard;
                        // the LHS slot's own cleanup stays the unique
                        // owner. Symmetric to the Identifier-RHS path
                        // above which zeroes the source-binding's cap.
                        if let Some(acc) = staged_fstr_acc {
                            self.zero_vec_alloca_cap(acc);
                        }
                    }
                } else if let ExprKind::FieldAccess { object, field } = &target.kind {
                    // Heap-env closure FIELD reassignment (`r.f = make(j)` /
                    // `r.f = g`): drop r.f's CURRENT env, inc the new env on a
                    // binding copy, store the new fat into the field slot — the
                    // binding-reassignment shape with the slot = field GEP. The
                    // field's scope-exit `FreeClosureEnv` then frees whatever is
                    // stored once. No-op (`false`) for any non-closure-field
                    // target, which falls through to the generic field store.
                    if self.try_compile_heap_env_field_reassign(object, field, val, value)? {
                        return Ok(());
                    }
                    self.compile_field_store(object, field, val, rhs_is_fresh)?;
                    // `cells[i].name = f"…"` — a heap String field store on a
                    // SoA element whose RHS is an f-string. `compile_soa_field_store`
                    // MOVES the f-string's buffer header into the group slot
                    // (and drops the old one), so the accumulator's own
                    // `FreeVecBuffer` (registered in the InterpolatedStringLit
                    // arm) must be neutralized — else both it and the SoA
                    // per-element drop free the same buffer (a double-free / the
                    // SIGTRAP this guards). The struct-literal field form is
                    // already covered by `suppress_fstr_acc_if_moved_out`; this
                    // is the direct-field-store peer. Gated to a SoA element
                    // field target so AoS field stores keep their existing
                    // (copy-based) acc handling.
                    if let Some(acc) = staged_fstr_acc {
                        if let ExprKind::Index { object: base, .. } = &object.kind {
                            if let ExprKind::Identifier(soa_name) = &base.kind {
                                if self.active_soa_layout(soa_name).is_some() {
                                    self.zero_vec_alloca_cap(acc);
                                }
                            }
                        }
                    }
                } else if let ExprKind::Index { object, index } = &target.kind {
                    // Heap-env closure Vec ELEMENT reassignment (`v[i] = make(j)` /
                    // `v[i] = g`): drop v[i]'s CURRENT env, inc the new env on a
                    // binding copy, store the new fat into the bounds-checked
                    // element slot — the binding-reassignment shape with the slot =
                    // the Vec element ptr. The Vec's dynamic (refcount-aware)
                    // element drop loop then frees whatever is stored once at scope
                    // exit. No-op (`false`) for any non-heap-env-Vec target, which
                    // falls through to the generic index store.
                    if self.try_compile_heap_env_vec_elem_reassign(object, index, val, value)? {
                        return Ok(());
                    }
                    self.compile_index_store(object, index, val)?;
                    // A tracked Vec/String binding moved into an OWNING Vec's
                    // heap-element slot (`out[j] = nb` where `out: Vec[Vec[T]]`)
                    // must have its scope-exit cleanup suppressed: the container
                    // now owns the buffer and frees it via its element-drop, so
                    // without this both the source binding and the container free
                    // it (double-free → SIGTRAP, B-2026-06-19-7). Mirrors the
                    // Identifier-assign move-suppression above and `Vec.push`'s
                    // `suppress_source_vec_cleanup_for_arg`. Gated to an owning
                    // Vec target with a heap-struct element type so a slice/map
                    // element store (borrowed / handle-owned) is untouched;
                    // `suppress_source_vec_cleanup_for_arg` is itself a no-op for
                    // a non-Identifier / non-tracked RHS.
                    if let ExprKind::Identifier(container) = &object.kind {
                        let owns_heap_elem = self
                            .vec_elem_types
                            .get(container.as_str())
                            .is_some_and(|&et| self.llvm_ty_is_vec_struct(et))
                            && !self.slice_elem_types.contains_key(container.as_str())
                            && !self.map_key_types.contains_key(container.as_str());
                        if owns_heap_elem {
                            self.suppress_source_vec_cleanup_for_arg(value);
                        }
                    }
                    // Whole-element SoA store of a NAMED owned struct binding
                    // (`grid[i] = c`): the scatter in `compile_soa_index_store`
                    // MOVED `c`'s fields — including any String/Vec buffer
                    // pointer — into the group buffers, so the SoA Vec owns
                    // them. Zero `c`'s heap-field caps so its `StructDrop`
                    // no-ops; otherwise both `c`'s drop and the SoA cleanup free
                    // the same buffer (a double-free ASAN catches). A struct-
                    // literal RHS has no source slot and is skipped; this is the
                    // SoA peer of push's move-in suppression and the
                    // `out[j] = nb` Vec[Vec] case above.
                    if let ExprKind::Identifier(container) = &object.kind {
                        if let Some(soa) = self.active_soa_layout(container) {
                            if let ExprKind::Identifier(src) = &value.kind {
                                if !self.ref_params.contains_key(src) {
                                    if let Some(src_slot) = self.variables.get(src).copied() {
                                        self.zero_struct_move_caps(src_slot.ptr, &soa.struct_name);
                                    }
                                }
                            }
                        }
                    }
                } else if let ExprKind::Unary {
                    op: UnaryOp::Deref,
                    operand,
                } = &target.kind
                {
                    // Raw-pointer store (`*p = val` with `p: *mut T`): the
                    // operand's *value* is the address, so compile it and store
                    // through it. `get_data_ptr`'s owned-local branch would hand
                    // back `p`'s own alloca and clobber the pointer variable
                    // instead of the pointee (B-2026-06-11-3 store side). The
                    // lowering side-table flags exactly the raw-pointer operands;
                    // mut-ref operands are absent and fall through below.
                    let key = (operand.span.offset, operand.span.length);
                    if self.raw_pointer_pointee_types.contains_key(&key) {
                        let ptr = self.compile_expr(operand)?.into_pointer_value();
                        self.builder.build_store(ptr, val).unwrap();
                    } else if let ExprKind::Identifier(name) = &operand.kind {
                        // `*r = val` — store through the mut-ref pointer.
                        // get_data_ptr loads the raw pointer from the alloca (one
                        // load, not two), giving us the address to store into.
                        if let Some(ptr) = self.get_data_ptr(name) {
                            self.builder.build_store(ptr, val).unwrap();
                        }
                    }
                }
                Ok(())
            }
            StmtKind::CompoundAssign { target, op, value } => {
                let binop = match op {
                    CompoundOp::Add => BinOp::Add,
                    CompoundOp::Sub => BinOp::Sub,
                    CompoundOp::Mul => BinOp::Mul,
                    CompoundOp::Div => BinOp::Div,
                    CompoundOp::Mod => BinOp::Mod,
                    CompoundOp::BitAnd => BinOp::BitAnd,
                    CompoundOp::BitOr => BinOp::BitOr,
                    CompoundOp::BitXor => BinOp::BitXor,
                    CompoundOp::Shl => BinOp::Shl,
                    CompoundOp::Shr => BinOp::Shr,
                };
                // `*r += rhs` / `r += rhs` where `r` is a let-bound entry slot
                // ref (`let r = m.entry(k).or_insert(d)`): load the slot pointer
                // from r's alloca, then load / apply / store back through it.
                if let Some(name) = self.entry_slot_ref_target(target) {
                    let (slot_ptr, val_ty) = self.entry_slot_ref_ptr(&name)?;
                    let cur = self
                        .builder
                        .build_load(val_ty, slot_ptr, "entry.ref.cur")
                        .unwrap();
                    let rhs = self.compile_expr(value)?;
                    let result = self.compile_binop(&binop, cur, rhs)?;
                    self.builder.build_store(slot_ptr, result).unwrap();
                    return Ok(());
                }
                // `*m.entry(k).or_insert(d) += rhs` — the canonical counter.
                // The entry chain lowers to a slot pointer (`mut ref V`) with
                // insert side effects, so compile it EXACTLY once, then load /
                // apply / store back through that pointer. (Compiling via
                // `compile_expr(target)` would either return the raw pointer or
                // re-emit the entry call, double-inserting.)
                if let ExprKind::Unary {
                    op: crate::ast::UnaryOp::Deref,
                    operand,
                } = &target.kind
                {
                    if let Some(map_name) = self.entry_chain_or_insert_map_name(operand) {
                        let val_ty = *self.map_val_types.get(&map_name).ok_or_else(|| {
                            format!("entry compound-assign: missing val type for '{}'", map_name)
                        })?;
                        let slot_ptr = self.compile_expr(operand)?.into_pointer_value();
                        let cur = self
                            .builder
                            .build_load(val_ty, slot_ptr, "entry.slot.cur")
                            .unwrap();
                        let rhs = self.compile_expr(value)?;
                        let result = self.compile_binop(&binop, cur, rhs)?;
                        self.builder.build_store(slot_ptr, result).unwrap();
                        return Ok(());
                    }
                }
                if let ExprKind::Identifier(name) = &target.kind {
                    // Slice 9: module-binding compound-assign loads
                    // through the global pointer (not the local
                    // variable map). `load_variable` errors when the
                    // name has no entry in `self.variables`; the
                    // module-binding fast path bypasses that — the
                    // load lowers to a direct LLVM `load` from the
                    // global.
                    let current = if let Some(loaded) = self.try_load_module_binding(name) {
                        loaded
                    } else {
                        self.load_variable(name)?
                    };
                    let rhs = self.compile_expr(value)?;
                    let result = self.compile_binop(&binop, current, rhs)?;
                    // Slice 9: module-binding compound-assign — store
                    // the binop's result back through the global. The
                    // load above (via `load_variable`) routes through
                    // the existing Identifier arm in `compile_expr`,
                    // which preferentially picks the module-binding
                    // path via `try_load_module_binding`. The store
                    // here mirrors that — `try_store_module_binding`
                    // short-circuits before the local-slot fallback.
                    if self.try_store_module_binding(name, result) {
                        return Ok(());
                    }
                    // Assign-through for a numeric-scalar `mut ref` param — the
                    // compound-assign sibling of the plain-Assign block above
                    // (see its comment for the full rationale). The alloca holds
                    // the borrow pointer; store the binop result THROUGH it
                    // (`get_data_ptr`) rather than into the alloca, so `x += 1`
                    // on a `mut ref i64` updates the caller's value.
                    if let Some(&inner_ty) = self.ref_params.get(name) {
                        if inner_ty.is_int_type() || inner_ty.is_float_type() {
                            if let Some(ptr) = self.get_data_ptr(name) {
                                let cval = self.coerce_scalar_to_type(result, inner_ty);
                                self.builder.build_store(ptr, cval).unwrap();
                                return Ok(());
                            }
                        }
                    }
                    if let Some(slot) = self.variables.get(name).copied() {
                        self.builder.build_store(slot.ptr, result).unwrap();
                    }
                    return Ok(());
                }
                // Field / index targets (`o.count += 1`, `o.inner.x += 1`,
                // `v[i] += 1`): read the current place value, apply the op, and
                // store the result back through the same place-store path as
                // plain Assign. Previously only the `Identifier` target was
                // handled, so compound assignment to a field / index was
                // silently dropped.
                let current = self.compile_expr(target)?;
                let rhs = self.compile_expr(value)?;
                let result = self.compile_binop(&binop, current, rhs)?;
                match &target.kind {
                    ExprKind::FieldAccess { object, field } => {
                        self.compile_field_store(object, field, result, false)?;
                    }
                    ExprKind::Index { object, index } => {
                        self.compile_index_store(object, index, result)?;
                    }
                    // `*x OP= rhs` for a value-represented `mut ref` (a closure
                    // `and_modify` param or a CICO fn `mut ref` param): the
                    // binding's alloca holds V directly, so store back to it —
                    // identical to the deref-elided `x OP= rhs`, and the
                    // writeback (closure exit / call site) propagates it. The
                    // read above (`compile_expr(target)`) already loaded V via
                    // `load_variable`. A pointer-represented `mut ref` (the rare
                    // `let r = m.entry(k).or_insert(d)`) is NOT handled here: its
                    // read yields a pointer and errors in `compile_binop` before
                    // reaching this store.
                    ExprKind::Unary {
                        op: crate::ast::UnaryOp::Deref,
                        operand,
                    } => {
                        if let ExprKind::Identifier(name) = &operand.kind {
                            if let Some(slot) = self.variables.get(name).copied() {
                                self.builder.build_store(slot.ptr, result).unwrap();
                            }
                        }
                    }
                    _ => {}
                }
                Ok(())
            }
            StmtKind::Defer { body } => {
                if let Some(frame) = self.scope_cleanup_actions.last_mut() {
                    frame.push(super::state::CleanupAction::UserDefer(body.clone()));
                }
                Ok(())
            }
            StmtKind::ErrDefer { binding, body } => {
                // Slice 2 shipped the no-binding form; slice 4 (Phase 7
                // § *defer / errdefer codegen*) lifts the
                // `binding.is_none()` gate so the binding form
                // `errdefer(e) { ... }` also lands on the unified
                // `scope_cleanup_actions` frame. Emission of the
                // binding form's payload-bind-then-run dispatch happens
                // in `emit_cleanup_action_at`'s
                // `UserErrDefer { binding: Some(_), .. }` arm, which
                // reads `pending_errdefer_payload` (staged by each
                // error-exit site immediately before
                // `emit_scope_cleanup_for_error_path`).
                if let Some(frame) = self.scope_cleanup_actions.last_mut() {
                    frame.push(super::state::CleanupAction::UserErrDefer {
                        binding: binding.clone(),
                        body: body.clone(),
                    });
                }
                Ok(())
            }
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => self.compile_let_else(pattern, value, else_block),
            // `LetUninit` falls through the catch-all below — its slot is
            // materialized lazily on first assignment, so a no-op is correct
            // there.
            _ => Ok(()),
        }
    }

    pub(super) fn bind_pattern(
        &mut self,
        pattern: &Pattern,
        val: BasicValueEnum<'ctx>,
    ) -> Result<(), String> {
        match &pattern.kind {
            PatternKind::Binding(name) => {
                // Type-changing shadow purge. A `let` / for-loop / match-arm /
                // destructure binding that re-binds a name already in scope
                // must not inherit the old binding's per-variable sidecar
                // metadata (`string_vars`, `vec_elem_types`, …, all keyed by
                // variable name); when the new binding has a different
                // type/class, a stale tag mis-dispatches a later use and traps
                // at runtime. The resolver and interpreter support such shadows
                // (design.md § Variables > Shadowing).
                //
                // `bind_pattern` is the single choke point for every binding
                // form, so purging here covers for-loop / match-arm / slice /
                // destructure shadows: those callers re-register the new
                // binding's metadata *after* `bind_pattern`
                // (`register_for_loop_bindings`, the match-arm payload
                // registration, `finish_owned_*_destructure`), so a full purge
                // here is exactly right for them.
                //
                // The `StmtKind::Let` arm is the exception: it writes the new
                // binding's metadata *before* `bind_pattern` and runs its own
                // take/restore dance (`shadow.rs`) to keep the OLD tags live
                // while the RHS may still reference the old binding
                // (`let s = s.len()`), then installs pure-NEW tags before the
                // bind. It sets `suppress_shadow_metadata_purge` so this purge
                // does not wipe those just-installed NEW tags.
                //
                // LeakSanitizer-safe: scope-exit drops are queued as
                // `CleanupAction`s keyed by the binding's alloca at bind time
                // (`scope_cleanup_actions`), not re-derived from these maps at
                // drain time, so the old binding still drops after its
                // name-metadata is forgotten.
                if !self.suppress_shadow_metadata_purge && self.variables.contains_key(name) {
                    self.forget_var_metadata(name);
                }
                let fn_val = self.current_fn.unwrap();
                let alloca = self.create_entry_alloca(fn_val, name, val.get_type());
                self.builder.build_store(alloca, val).unwrap();
                self.variables.insert(
                    name.clone(),
                    VarSlot {
                        ptr: alloca,
                        ty: val.get_type(),
                    },
                );
                // Phase 6 "Channel AOT codegen lowering": register a channel
                // end (`Sender`/`Receiver`) bound here for scope-exit refcount
                // drop. Keyed off the typechecker's `pattern_binding_types`
                // (span-stable — unlike `var_type_names`, which the
                // statement-hoisting pre-pass resets), so it fires for both
                // the `let (tx, rx) = Channel.new()` destructure and the
                // single-binding `let tx2 = tx.clone()` (both funnel through
                // this arm). The matching `karac_runtime_channel_new` returns
                // refcount 2 and `clone` increments, so one drop per binding
                // balances the channel's lifetime to zero.
                let key = (pattern.span.offset, pattern.span.length);
                if let Some(surface) = self.pattern_binding_types.get(&key) {
                    // `Sender` drop may close the channel (waking blocked
                    // receivers); `Receiver` drop only releases its reference.
                    if surface == "Sender" || surface == "Receiver" {
                        let is_sender = surface == "Sender";
                        self.track_channel_var(alloca, is_sender);
                    }
                }
                Ok(())
            }
            PatternKind::Wildcard => Ok(()),
            // Struct destructuring: let Foo { x, y } = val
            PatternKind::Struct {
                path: _,
                fields,
                has_rest: _,
            } => {
                if let BasicValueEnum::StructValue(sv) = val {
                    for (idx, field_pat) in fields.iter().enumerate() {
                        let field_val = self
                            .builder
                            .build_extract_value(sv, idx as u32, "field")
                            .unwrap();
                        if let Some(pat) = &field_pat.pattern {
                            self.bind_pattern(pat, field_val)?;
                        } else {
                            // Shorthand `Foo { x }` — bind field name as variable
                            let fn_val = self.current_fn.unwrap();
                            let alloca = self.create_entry_alloca(
                                fn_val,
                                &field_pat.name,
                                field_val.get_type(),
                            );
                            self.builder.build_store(alloca, field_val).unwrap();
                            self.variables.insert(
                                field_pat.name.clone(),
                                VarSlot {
                                    ptr: alloca,
                                    ty: field_val.get_type(),
                                },
                            );
                        }
                    }
                }
                Ok(())
            }
            // Tuple destructuring: let (a, b) = val
            PatternKind::Tuple(pats) => {
                if let BasicValueEnum::StructValue(sv) = val {
                    for (idx, pat) in pats.iter().enumerate() {
                        let elem = self
                            .builder
                            .build_extract_value(sv, idx as u32, "elem")
                            .unwrap();
                        self.bind_pattern(pat, elem)?;
                        // B-2026-06-12-3: register method-dispatch side-tables
                        // for each Binding leaf of the tuple. `bind_pattern`
                        // only allocated the slot, so a String/Vec/Slice element
                        // (`let (inner, after) = decode_at(...)`) had no
                        // `string_vars` / `vec_elem_types` entry and
                        // `inner.repeat(k)` failed codegen with "no handler for
                        // method 'repeat' on variable 'inner'" — while the
                        // interpreter handled it. This is the tuple counterpart
                        // of `finish_owned_struct_destructure` (which already
                        // registers struct-destructure fields). Scoped to the
                        // destructure leaf — NOT the top-level single `let x`,
                        // which the main let handler registers — so it can't
                        // clobber an already-correct registration. Nested tuples
                        // recurse through this same arm. Dispatch-only: no
                        // cleanup tracking (the destructure's heap ownership is
                        // handled elsewhere), so it never double-frees.
                        if let PatternKind::Binding(name) = &pat.kind {
                            self.register_pattern_leaf_dispatch(name, &pat.span);
                        }
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Register method-dispatch side-tables for one `let`-destructure leaf
    /// binding, keyed off the typechecker's span-stable `pattern_binding_types`
    /// (surface name) + `pattern_binding_inner_types` (inner element `TypeExpr`
    /// for `Vec`/`Slice`). This is the `bind_pattern` (let path) counterpart of
    /// the registration the match/if-let binder `bind_pattern_values` already
    /// performs — without it, `let (a, _) = pair(); a.repeat(2)` (and any
    /// String/Vec/Slice method on a tuple/struct-destructured element) fails
    /// codegen with "no handler for method …" (B-2026-06-12-3).
    ///
    /// **Dispatch-only.** It populates type registries (`string_vars`,
    /// `vec_elem_types`, `slice_elem_types`, Map/Set tables via
    /// `register_var_from_type_expr`, else `var_type_names`) but queues NO
    /// scope-exit cleanup — the destructure's heap ownership is handled by its
    /// own path, so adding a per-leaf free here would double-free. All inserts
    /// are idempotent, so the single-`let x = …` path (which also reaches this
    /// arm and is already registered by the main handler) is unaffected.
    fn register_pattern_leaf_dispatch(&mut self, name: &str, span: &crate::token::Span) {
        let key = (span.offset, span.length);
        let Some(type_name) = self.pattern_binding_types.get(&key).cloned() else {
            return;
        };
        match type_name.as_str() {
            // String layout matches `Vec[u8]` (`{ptr,len,cap}`); register both
            // the element type and the `string_vars` flag the String method
            // arms gate on — mirrors the single-`let` String registration.
            "String" => {
                self.vec_elem_types
                    .insert(name.to_string(), self.context.i8_type().into());
                self.string_vars.insert(name.to_string());
            }
            // `VecDeque[T]` shares `Vec[T]`'s storage + dispatch.
            "Vec" | "VecDeque" => {
                if let Some(inner_te) = self.pattern_binding_inner_types.get(&key).cloned() {
                    let elem_llvm = self.llvm_type_for_type_expr(&inner_te);
                    self.vec_elem_types.insert(name.to_string(), elem_llvm);
                    self.var_elem_type_exprs.insert(name.to_string(), inner_te);
                }
            }
            "Slice" => {
                if let Some(inner_te) = self.pattern_binding_inner_types.get(&key).cloned() {
                    let elem_llvm = self.llvm_type_for_type_expr(&inner_te);
                    self.slice_elem_types.insert(name.to_string(), elem_llvm);
                    self.var_elem_type_exprs.insert(name.to_string(), inner_te);
                }
            }
            // Map/Set: when the full collection `TypeExpr` is available, route
            // through the shared registrar (extracts K/V/elem). No-op when the
            // inner-types table doesn't carry it (the let path may not).
            "Map" | "Set" => {
                if let Some(full_te) = self.pattern_binding_inner_types.get(&key).cloned() {
                    self.register_var_from_type_expr(name, &full_te);
                }
            }
            // User struct / shared handle / other: record the surface name so
            // field access + method dispatch resolve the right shape.
            _ => {
                self.record_var_type_name(name.to_string(), type_name);
            }
        }
    }

    /// Finish an owned `let Point { … } = <expr>` destructure: register
    /// method-dispatch side-tables for each bound field and queue scope-exit
    /// cleanup for the heap-owning ones. `bind_pattern` only allocas the field
    /// bindings; without this they could neither dispatch methods
    /// (`items.len()` → "no handler for method") nor free their heap (the
    /// struct-destructure leak). B follow-up #3 —
    /// docs/spikes/pattern-arm-unbound-field-drop.md.
    ///
    /// Dispatch registration runs for every bound field (harmless; it only
    /// populates side-tables). Cleanup runs only when the RHS is a *fresh
    /// owned temporary* (`make()` etc.): a fresh temp has no source binding, so
    /// each heap field is owned outright by its new binding, or orphaned (a
    /// field left unbound by `_` / a `..` rest) — freeing it here is the only
    /// free. A non-fresh RHS (`let Point { … } = p`) keeps today's behavior:
    /// `p`'s own cleanup owns the heap, so a second free would double-free;
    /// that case stays a (pre-existing) dispatch-only gap. Structs have static
    /// field offsets, so each field gets its own one-shot cleanup — no
    /// whole-value drop + cap-suppression dance (the enum B path needs that
    /// only because the live variant is dynamic).
    /// True iff some in-scope cleanup frame holds a `StructDrop` whose
    /// `struct_alloca` is `ptr` — i.e. the struct local/param rooted at `ptr`
    /// owns its heap fields and will free them at scope exit (the callee-owned
    /// param case, #14/#17). Used to distinguish a destructure whose source has
    /// its OWN drop (transfer ownership to leaves) from one covered by a parent
    /// drop (a match-binding payload — keep source-owns).
    fn ptr_has_registered_struct_drop(&self, ptr: PointerValue<'ctx>) -> bool {
        self.scope_cleanup_actions.iter().any(|frame| {
            frame.iter().any(|a| {
                matches!(
                    a,
                    super::state::CleanupAction::StructDrop { struct_alloca, .. }
                        if *struct_alloca == ptr
                )
            })
        })
    }

    fn finish_owned_struct_destructure(
        &mut self,
        pattern: &Pattern,
        value: &Expr,
        val: BasicValueEnum<'ctx>,
    ) -> Result<(), String> {
        let PatternKind::Struct { path, fields, .. } = &pattern.kind else {
            return Ok(());
        };
        let struct_name = path.last().cloned().unwrap_or_default();
        let Some(field_names) = self.struct_field_names.get(&struct_name).cloned() else {
            return Ok(());
        };
        let Some(field_tes) = self.struct_field_type_exprs.get(&struct_name).cloned() else {
            return Ok(());
        };
        let BasicValueEnum::StructValue(sv) = val else {
            return Ok(());
        };
        let fresh = self.expr_yields_fresh_owned_temp(value);

        // Place-source (`let S { a, b } = s`) where the source struct `s` is
        // CALLEE-OWNED — a bare by-value param deep-copied at entry (#14/#17
        // `make_aggregate_param_callee_owned`), so it carries its OWN scope-exit
        // `StructDrop`. The destructure bindings alias the source's (copied)
        // fields, so the source's drop AND any binding move-out both free the
        // same buffer — a double-free (selfhost slice 3c-ii: `render_variant`'s
        // `let VariantNode { .. } = v` then a consumed `Vec` field, and the
        // minimal `fn f(s: S) -> String { let S { a, b } = s; a }`). Transfer
        // ownership of each Vec/String/non-shared-struct field to its leaf
        // binding (track its own cleanup) and cap-zero that field in the source
        // so the `StructDrop` skips it — the struct analog of
        // `finish_place_source_tuple_destructure` (#21), extended to Vec/String
        // leaves. Gated on the source carrying its OWN registered `StructDrop`,
        // so a match-binding payload source (`match t { Foo(n) => { let Bar { .. }
        // = n } }`, whose payload is freed by the enum's drop, NOT its own) keeps
        // the proven source-owns behavior. Shared / Map / Set / enum / scalar
        // fields stay on their existing paths (RC / handle / payload-overlay).
        let callee_owned_src: Option<PointerValue<'ctx>> = if fresh {
            None
        } else if let ExprKind::Identifier(root) = &value.kind {
            self.variables
                .get(root.as_str())
                .copied()
                .filter(|slot| self.ptr_has_registered_struct_drop(slot.ptr))
                .map(|slot| slot.ptr)
        } else {
            None
        };

        // B-2026-07-09-12 clone-on-extract — is the source a shared-enum-payload
        // VIEW (`match e { Call(c) => { let CallNode { .. } = c } }`)? Unlike a
        // callee-owned source it has NO registered struct-drop; the box's rc-drop
        // owns its heap. Each extracted leaf therefore ALIASES the box's heap and
        // must be DUPLICATED so the leaf owns it independently (below).
        let view_src: bool = !fresh
            && callee_owned_src.is_none()
            && matches!(&value.kind, ExprKind::Identifier(root)
                if self.shared_enum_payload_view_vars.contains_key(root.as_str()));

        for (idx, fname) in field_names.iter().enumerate() {
            let Some(field_te) = field_tes.get(idx).cloned() else {
                continue;
            };
            // Nested struct pattern (`inner: Inner { data }`): `bind_pattern`
            // already allocated the nested leaf bindings, but their dispatch
            // side-tables were never registered (so `data.len()` failed with
            // "no handler for method"). Recurse to register dispatch for every
            // nested leaf. Dispatch-only — per-leaf CLEANUP for nested fields
            // stays a tracked narrow leak: the enclosing field is freed as one
            // unit by the `else` discard branch below, so its heap is still
            // freed once; what's missing is per-nested-leaf move-out precision.
            if let Some(p) = fields
                .iter()
                .find(|f| &f.name == fname)
                .and_then(|f| f.pattern.as_ref())
            {
                if matches!(&p.kind, PatternKind::Struct { .. }) {
                    if let TypeKind::Path(tp) = &field_te.kind {
                        if let Some(nested) = tp.segments.last().cloned() {
                            self.register_struct_pattern_dispatch(&nested, p);
                        }
                    }
                }
            }
            // Which name (if any) does the pattern bind this field to?
            let bound_name: Option<String> = match fields.iter().find(|f| &f.name == fname) {
                Some(f) => match &f.pattern {
                    None => Some(f.name.clone()),
                    Some(p) => match &p.kind {
                        PatternKind::Binding(n) => Some(n.clone()),
                        // `_` (Wildcard) or a nested pattern — not a plain
                        // owned leaf binding; treat as unbound here.
                        _ => None,
                    },
                },
                // Absent from the pattern: dropped by a `..` rest — unbound.
                None => None,
            };

            if let Some(name) = bound_name {
                // Dispatch always (so `field.method()` compiles for any RHS).
                self.register_var_from_type_expr(&name, &field_te);
                if fresh && self.destructure_field_needs_cleanup(&field_te) {
                    if let Some(slot) = self.variables.get(&name).copied() {
                        self.track_owned_destructure_field_cleanup(&name, slot.ptr, &field_te);
                    }
                } else if let Some(src_ptr) = callee_owned_src {
                    // Callee-owned place source (see `callee_owned_src` above):
                    // transfer Vec/String/non-shared-struct fields — the kinds
                    // `zero_struct_field_move_cap` zeroes — to the leaf binding,
                    // and cap-zero that field in the source so its `StructDrop`
                    // skips it. Shared/Map/Set/enum/scalar fields stay on their
                    // existing source-owns / RC / handle paths.
                    let transferable = self.extract_vec_elem_type(&field_te).is_some()
                        || self.is_string_type_expr(&field_te)
                        // B-2026-07-03-28 Facet A — an `Option[inline-heap]` field
                        // of a CALLEE-OWNED struct (the source has a registered
                        // struct-drop, i.e. it is entry-copied): transfer its
                        // payload to the leaf (track the leaf's inline-Option
                        // cleanup) and zero the SOURCE tag so the source
                        // struct-drop's `OptionInline` free skips it.
                        || self.option_inline_payload_elem(&field_te).is_some()
                        || matches!(
                            &field_te.kind,
                            TypeKind::Path(p) if p.segments.last().is_some_and(|s|
                                self.struct_types.contains_key(s.as_str())
                                    && !self.shared_types.contains_key(s.as_str()))
                        );
                    if transferable {
                        if let Some(slot) = self.variables.get(&name).copied() {
                            self.track_owned_destructure_field_cleanup(&name, slot.ptr, &field_te);
                        }
                        self.zero_struct_field_move_cap(src_ptr, &struct_name, fname);
                    }
                } else if view_src {
                    // B-2026-07-09-12 clone-on-extract — the source is a shared-enum
                    // payload VIEW; the leaf aliases the box's heap. Duplicate it in
                    // place (deep-copy a buffer / rc-inc a shared handle) so the leaf
                    // owns it independently and the box's rc-drop does not double-free
                    // the moved-out child, then register the leaf's own cleanup.
                    // Per-field: an unsupported shape (Vec[shared] / Vec[agg] /
                    // Option / Map / Set) is left as the status-quo view alias.
                    if let Some(slot) = self.variables.get(&name).copied() {
                        self.clone_on_extract_view_field(&name, slot.ptr, &field_te);
                    }
                }
                // B-2026-07-03-27 — an `Option[<user struct/enum>]` field. The
                // branches above don't cover it: `destructure_field_needs_cleanup`
                // excludes `Option`, and `transferable` (Vec/String/struct) does
                // too. Struct drop NEVER frees an `Option` field (excluded by
                // design — B-2026-07-03-28, blocked), so when this destructure
                // OWNS the source (a fresh temp, or a moved-in owned binding —
                // NOT a `ref`/borrow, which still owns the payload elsewhere) the
                // leaf must free the `Some` payload or it leaks. Independent of
                // the branches above (whose `track_owned_destructure_field_cleanup`
                // has no `Option` arm), so no double registration. Runs for both
                // the fresh and callee-owned sources — the field's own `Option`
                // drop is orthogonal to the source's `StructDrop` (which skips it).
                if self.option_field_agg_drop_ok(&field_te) && !view_src {
                    let source_owned = fresh
                        || matches!(&value.kind, ExprKind::Identifier(n)
                            if !self.ref_params.contains_key(n.as_str()));
                    if source_owned {
                        if let Some(slot) = self.variables.get(&name).copied() {
                            self.track_inline_option_agg_payload_var(&name, slot.ptr, &field_te);
                        }
                        // B-2026-07-04-7 — the comment above ("Struct drop NEVER
                        // frees an `Option` field") no longer holds: a copy-supported
                        // struct with an `Option[<struct/enum>]` field now carries an
                        // `OptionInline` struct-drop for it. When the destructure
                        // source is CALLEE-OWNED (entry-copied, has its own
                        // `StructDrop`), zero its tag so that drop skips the
                        // moved-out payload — else it double-frees against this
                        // leaf's own `Option` drop (B-27/B-31 exit-133). A fresh-temp
                        // source has no lingering struct-drop, so nothing to disarm.
                        if let Some(src_ptr) = callee_owned_src {
                            self.zero_struct_field_move_cap(src_ptr, &struct_name, fname);
                        }
                    }
                }
            } else if fresh && self.destructure_field_needs_cleanup(&field_te) {
                // Unbound heap field (`items: _` or dropped by `..`): no
                // binding to free it, so stash a copy in a synthetic slot and
                // queue its cleanup — otherwise the buffer leaks.
                let field_val = self
                    .builder
                    .build_extract_value(sv, idx as u32, "destructure.discard")
                    .unwrap();
                let fn_val = self.current_fn.unwrap();
                let synth = format!("__destructure_discard_{}", self.indexed_elem_counter);
                self.indexed_elem_counter += 1;
                let alloca = self.create_entry_alloca(fn_val, &synth, field_val.get_type());
                self.builder.build_store(alloca, field_val).unwrap();
                self.register_var_from_type_expr(&synth, &field_te);
                self.track_owned_destructure_field_cleanup(&synth, alloca, &field_te);
            }
        }
        Ok(())
    }

    /// B-2026-07-09-12 clone-on-extract — duplicate a single destructure LEAF that
    /// was moved out of a shared-enum-payload VIEW so it owns its heap
    /// independently of the box, then register the leaf's own scope-exit cleanup.
    /// The RC box's rc-drop stays the sole owner of the ORIGINAL, so nothing
    /// double-frees. Handles the shapes whose copy the drop exactly balances:
    /// bare `shared` (rc-inc), String / `Vec[heap-free elem]` (buffer deep-copy),
    /// and a nested fully-clone-duplicable struct. Any other shape (`Vec[shared]`,
    /// `Vec[String/agg]`, `Option`, `Map`, `Set`, a non-duplicable struct) is left
    /// as the pre-existing view alias — no worse than before, just not yet
    /// move-out-capable (the residual tail of this bug).
    fn clone_on_extract_view_field(
        &mut self,
        var_name: &str,
        leaf_ptr: PointerValue<'ctx>,
        field_te: &TypeExpr,
    ) {
        // Bare `shared` handle → rc-inc + scope-exit rc-dec. The leaf's own dec
        // (or its consumer's, when moved) balances the inc; the box's rc-drop
        // balances the box's original ref.
        if let Some(heap_type) = self.shared_heap_type_for_type_expr(field_te) {
            self.rc_inc_shared_handle_at_slot(leaf_ptr, heap_type);
            self.track_rc_var(var_name, leaf_ptr, heap_type);
            return;
        }
        let vec_ty = self.vec_struct_type();
        // String → deep-copy the buffer + track (element-free, so outer copy is
        // complete).
        if self.is_string_type_expr(field_te) {
            let i8t = self.context.i8_type().into();
            if let Ok(val) = self.builder.build_load(vec_ty, leaf_ptr, "viewdup.str") {
                let copied = self.emit_vecstr_defensive_copy(val, i8t, None);
                let _ = self.builder.build_store(leaf_ptr, copied);
            }
            self.track_vec_var(leaf_ptr, Some(i8t));
            return;
        }
        // `Vec[shared]` (`Block.stmts: Vec[Stmt]`, `CallExpr.args: Vec[Expr]`) →
        // deep-copy the outer buffer, rc-INC each element box, and register the
        // per-element rc-dec drop (`track_owned_destructure_field_cleanup` routes a
        // `Vec[shared]` to `emit_vec_elem_rc_dec_fn`). The leaf then independently
        // co-owns every element; the box's rc-drop keeps its originals.
        if let Some(inner) = crate::codegen::helpers::vec_inner_type_expr(field_te) {
            if let Some(heap_type) = self.shared_heap_type_for_type_expr(&inner) {
                if let Some(elem_ty) = self.extract_vec_elem_type(field_te) {
                    if let Ok(val) = self.builder.build_load(vec_ty, leaf_ptr, "viewdup.vshared") {
                        let copied = self.emit_vecstr_defensive_copy(val, elem_ty, None);
                        let _ = self.builder.build_store(leaf_ptr, copied);
                    }
                    self.rc_inc_vec_shared_elements(leaf_ptr, heap_type);
                    self.track_owned_destructure_field_cleanup(var_name, leaf_ptr, field_te);
                    return;
                }
            }
        }
        // Vec whose element carries NO heap of its own (`Vec[i64]`, `Vec[bool]`) →
        // the outer `{ptr,len,cap}` deep-copy is a complete duplicate. A
        // `Vec[String]` / `Vec[agg]` element still aliases the box's per-element
        // heap after an outer copy, so bail on those.
        if let Some(elem_ty) = self.extract_vec_elem_type(field_te) {
            let elem_has_own_heap = crate::codegen::helpers::vec_inner_type_expr(field_te)
                .map(|e| {
                    self.type_expr_has_drop_heap(&e)
                        || self.shared_heap_type_for_type_expr(&e).is_some()
                })
                .unwrap_or(true);
            if elem_has_own_heap {
                return;
            }
            if let Ok(val) = self.builder.build_load(vec_ty, leaf_ptr, "viewdup.vec") {
                let copied = self.emit_vecstr_defensive_copy(val, elem_ty, None);
                let _ = self.builder.build_store(leaf_ptr, copied);
            }
            self.track_vec_var(leaf_ptr, Some(elem_ty));
            return;
        }
        // `Option[shared]` (`Block.tail: Option[Expr]`, `IfNode.else: Option[Expr]`)
        // → rc-INC the `Some` box in place (`deep_copy_option_inline_payload_in_place`'s
        // shared leg) and register the tag-guarded scope-exit rc-dec +
        // move-suppression metadata (`track_rc_option_var`, which also records
        // `var_option_shared_heap` so a consumed/reassigned leaf is balanced). The
        // box's rc-drop keeps its own `Some` ref.
        if let Some((_, inner_info)) = self.option_inner_shared_type_for_type_expr(field_te) {
            self.deep_copy_option_inline_payload_in_place(leaf_ptr, field_te);
            if let Some(option_ty) = self.enum_layouts.get("Option").map(|l| l.llvm_type) {
                self.track_rc_option_var(var_name, leaf_ptr, option_ty, inner_info.heap_type);
            }
            return;
        }
        // Nested fully-clone-duplicable struct (String / Vec[heap-free] / nested
        // such — the `struct_clone_fully_duplicates` shape, whose
        // `deep_copy_struct_heap_fields_in_place` and struct-drop are symmetric).
        if let TypeKind::Path(p) = &field_te.kind {
            if let Some(head) = p.segments.first() {
                if self.struct_types.contains_key(head.as_str())
                    && !self.shared_types.contains_key(head.as_str())
                    && self.struct_clone_fully_duplicates(head.as_str(), &mut Vec::new())
                {
                    let name = head.clone();
                    self.deep_copy_struct_heap_fields_in_place(leaf_ptr, &name);
                    self.track_struct_var(&name, leaf_ptr);
                }
            }
        }
        // Any other shape: leave the status-quo view alias.
    }

    /// Finish an owned `let (a, b) = <expr>` tuple destructure: queue scope-exit
    /// cleanup for each heap-owning leaf. `bind_pattern` only allocates the leaf
    /// slots (and B-12-3 registers their dispatch side-tables) — without this,
    /// a `String`/`Vec` element's heap buffer leaks once per destructure
    /// (B-2026-06-13-5). The tuple sibling of `finish_owned_struct_destructure`.
    ///
    /// Gated on `expr_yields_fresh_owned_temp(value)`: only a fresh owned temp
    /// (call/method result, not a borrow) hands its element buffers to the
    /// leaves. A move/alias of an existing tuple binding (`let (a, b) = t`) is
    /// freed by its source, so tracking here would double-free. The general
    /// move-out suppression keys on the leaf slot, so a leaf later returned or
    /// moved into a sink is suppressed exactly like a simple `let` binding.
    fn finish_owned_tuple_destructure(
        &mut self,
        pattern: &Pattern,
        value: &Expr,
        val: BasicValueEnum<'ctx>,
    ) -> Result<(), String> {
        let PatternKind::Tuple(pats) = &pattern.kind else {
            return Ok(());
        };
        let BasicValueEnum::StructValue(sv) = val else {
            return Ok(());
        };
        if !self.expr_yields_fresh_owned_temp(value) {
            // #21 — a PLACE source (`let (t, n) = h.pe`): the source struct's
            // `NestedTuple` drop now frees the tuple's enum / nested-struct
            // leaves. The fresh-temp path below tracks ALL leaves; here we touch
            // ONLY those newly-freed leaf kinds — register + track each (so an
            // UNUSED leaf frees itself, not leaks) and cap-zero that element in
            // the SOURCE (so a CONSUMED leaf's binding/match is the sole owner).
            // Vec/String/i64 leaves keep the proven source-owns model untouched.
            self.finish_place_source_tuple_destructure(pats, value);
            return Ok(());
        }
        self.track_tuple_destructure_leaf_cleanups(pats, sv);
        Ok(())
    }

    /// #21 — the place-source half of [`Self::finish_owned_tuple_destructure`]
    /// (`let (t, n) = h.pe` where `h` is an owned local/callee-owned struct).
    /// For each leaf whose element type is a non-shared user ENUM or nested
    /// STRUCT — the kinds the owning struct's `NestedTuple` drop newly frees —
    /// register + track the leaf and cap-zero that element in the source so the
    /// struct drop skips it. Bails on a caller-retains root (`owned_struct_params`,
    /// whose deep-copy owns the buffer) or an unresolvable source.
    fn finish_place_source_tuple_destructure(&mut self, pats: &[Pattern], value: &Expr) {
        match Self::place_root_ident(value) {
            Some(root) if self.owned_struct_params.contains(root) => return,
            Some(_) => {}
            None => return,
        }
        let Some(elems) = self.place_chain_tuple_tes(value) else {
            return;
        };
        let Some(base_ptr) = self.field_chain_place_ptr(value) else {
            return;
        };
        let Some(tuple_ty) = self.place_chain_aggregate_llvm_type(value) else {
            return;
        };
        for (idx, pat) in pats.iter().enumerate() {
            let PatternKind::Binding(name) = &pat.kind else {
                continue;
            };
            let Some(te) = elems.get(idx).cloned() else {
                continue;
            };
            // Only ENUM / nested-STRUCT leaves are newly freed by `NestedTuple`;
            // Vec/String leaves keep the existing source-owns behavior.
            let TypeKind::Path(p) = &te.kind else {
                continue;
            };
            let Some(leaf_name) = p.segments.last().map(|s| s.as_str()) else {
                continue;
            };
            let is_enum = leaf_name != "Option"
                && leaf_name != "Result"
                && self
                    .enum_layouts
                    .get(leaf_name)
                    .is_some_and(|l| !l.is_shared);
            let is_struct = self.struct_types.contains_key(leaf_name)
                && !self.shared_types.contains_key(leaf_name);
            if !is_enum && !is_struct {
                continue;
            }
            self.register_var_from_type_expr(name, &te);
            if let Some(slot) = self.variables.get(name.as_str()).copied() {
                self.track_destructure_leaf_cleanup(name, slot.ptr);
            }
            self.zero_tuple_elem_cap_at(base_ptr, tuple_ty, idx as u32, &te);
        }
    }

    /// Per-element cleanup for an owned (already-fresh-checked) tuple
    /// destructure. Split out so nested tuples (`let (a, (b, c)) = …`) recurse.
    fn track_tuple_destructure_leaf_cleanups(
        &mut self,
        pats: &[Pattern],
        sv: inkwell::values::StructValue<'ctx>,
    ) {
        for (idx, pat) in pats.iter().enumerate() {
            match &pat.kind {
                PatternKind::Binding(name) => {
                    if let Some(slot) = self.variables.get(name.as_str()).copied() {
                        self.track_destructure_leaf_cleanup(name, slot.ptr);
                    }
                }
                // `let (a, _) = pair()` — the discarded element still owns its
                // buffer. A wildcard has no binding / type-table entry, so detect
                // the String/Vec shape by LLVM layout (the `{ptr,len,cap}` vec
                // struct) and free it through a synthetic slot. (A discarded
                // `Vec[String]` frees its outer buffer here; per-element inner
                // frees of a discarded nested-heap collection are a narrow
                // residual, as in the struct path.)
                PatternKind::Wildcard => {
                    let elem = self
                        .builder
                        .build_extract_value(sv, idx as u32, "tuple.discard")
                        .unwrap();
                    if elem.get_type() == self.vec_struct_type().into() {
                        let fn_val = self.current_fn.unwrap();
                        let synth = format!("__tuple_discard_{}", self.indexed_elem_counter);
                        self.indexed_elem_counter += 1;
                        let alloca = self.create_entry_alloca(fn_val, &synth, elem.get_type());
                        self.builder.build_store(alloca, elem).unwrap();
                        let i8t = self.context.i8_type().into();
                        self.track_vec_var(alloca, Some(i8t));
                    }
                }
                // Nested tuple: recurse (the whole aggregate was already proven
                // fresh by the caller, so each nested element is fresh too).
                PatternKind::Tuple(inner) => {
                    let elem = self
                        .builder
                        .build_extract_value(sv, idx as u32, "tuple.nested")
                        .unwrap();
                    if let BasicValueEnum::StructValue(inner_sv) = elem {
                        self.track_tuple_destructure_leaf_cleanups(inner, inner_sv);
                    }
                }
                // Nested struct pattern inside a tuple (`let (a, Foo { x }) = …`):
                // dispatch was registered by `bind_pattern`, but per-field cleanup
                // there is not wired (a narrow residual; the reported tuple-of-
                // String/Vec leak is fully covered).
                _ => {}
            }
        }
    }

    /// Queue scope-exit cleanup for a heap-owning destructure leaf, keyed off
    /// the dispatch side-tables `register_pattern_leaf_dispatch` already
    /// populated: String/Vec → `{ptr,len,cap}` buffer free; Map/Set → handle
    /// free; owned (non-shared) struct → its drop fn. `Slice` leaves are borrows
    /// (registered in `slice_elem_types`, not `vec_elem_types`) so they queue
    /// nothing. This is the registry-keyed analogue of
    /// `track_owned_destructure_field_cleanup` (which keys off a `TypeExpr` the
    /// tuple path doesn't have without an annotation).
    fn track_destructure_leaf_cleanup(&mut self, name: &str, alloca: PointerValue<'ctx>) {
        // String + Vec both register `vec_elem_types` (the buffer shape).
        if let Some(&elem) = self.vec_elem_types.get(name) {
            self.track_vec_var(alloca, Some(elem));
            return;
        }
        if self.map_key_types.contains_key(name) || self.set_elem_types.contains_key(name) {
            let key_is_vec = self
                .map_key_types
                .get(name)
                .or_else(|| self.set_elem_types.get(name))
                .copied()
                .is_some_and(|t| self.llvm_ty_is_vec_struct(t));
            let val_is_vec = self
                .map_val_types
                .get(name)
                .copied()
                .is_some_and(|t| self.llvm_ty_is_vec_struct(t));
            let val_shared_heap = self.map_val_shared_heap_type_for(name);
            let key_shared_heap = self.map_key_shared_heap_type_for(name);
            // Slice 3r: per-value drop fn for non-overlay heap values.
            let val_drop_fn = self
                .var_elem_type_exprs
                .get(name)
                .cloned()
                .and_then(|vte| self.map_val_drop_fn_for_type_expr(&vte));
            let (val_is_vec, val_shared_heap) = if val_drop_fn.is_some() {
                (false, None)
            } else {
                (val_is_vec, val_shared_heap)
            };
            self.track_map_var_with_val_drop(
                alloca,
                key_is_vec,
                val_is_vec,
                val_shared_heap,
                key_shared_heap,
                val_drop_fn,
            );
            return;
        }
        if let Some(tn) = self.var_type_names.get(name).cloned() {
            // #21 — a non-shared user-enum leaf (`let (t, n) = h.pe` with `t: Tok`):
            // track its `EnumDrop` so an unused leaf frees its payload (a consumed
            // leaf's `match` suppresses this drop). Without this the leaf was
            // untracked and relied on the source struct's drop — which now
            // (NestedTuple) the destructure cap-zeros, so the leaf must own.
            if let Some(layout) = self.enum_layouts.get(&tn) {
                if !layout.is_shared {
                    self.track_enum_var(&tn, alloca);
                    return;
                }
            }
            if self.struct_types.contains_key(&tn) && !self.shared_types.contains_key(&tn) {
                self.track_struct_var(&tn, alloca);
            }
        }
    }

    /// Recursively register method-dispatch side-tables for the leaf bindings
    /// of a (possibly nested) struct pattern — `bind_pattern` allocates the
    /// nested leaves but leaves them dispatch-less, so without this
    /// `let Outer { inner: Inner { data } } = …; data.len()` fails with "no
    /// handler for method 'len' on variable 'data'". Dispatch-only: it just
    /// populates the `register_var_from_type_expr` side-tables (Vec/Map/Set/
    /// struct), exactly like the top-level leaves in
    /// `finish_owned_struct_destructure`. Per-nested-leaf cleanup precision
    /// stays a tracked narrow leak (the enclosing field frees its heap as one
    /// unit). Tuple / enum sub-patterns inside a struct field are not walked
    /// here (separate follow-ups) — only struct-in-struct nesting.
    fn register_struct_pattern_dispatch(&mut self, struct_name: &str, pattern: &Pattern) {
        let PatternKind::Struct { fields, .. } = &pattern.kind else {
            return;
        };
        let Some(field_names) = self.struct_field_names.get(struct_name).cloned() else {
            return;
        };
        let Some(field_tes) = self.struct_field_type_exprs.get(struct_name).cloned() else {
            return;
        };
        for f in fields {
            let Some(idx) = field_names.iter().position(|n| n == &f.name) else {
                continue;
            };
            let Some(field_te) = field_tes.get(idx).cloned() else {
                continue;
            };
            match &f.pattern {
                // Shorthand leaf (`Inner { data }`): the field name is the var.
                None => self.register_var_from_type_expr(&f.name, &field_te),
                Some(p) => match &p.kind {
                    // `field: x` leaf — bind `x`.
                    PatternKind::Binding(n) => {
                        let n = n.clone();
                        self.register_var_from_type_expr(&n, &field_te);
                    }
                    // Deeper struct nesting — recurse.
                    PatternKind::Struct { .. } => {
                        if let TypeKind::Path(tp) = &field_te.kind {
                            if let Some(nested) = tp.segments.last().cloned() {
                                self.register_struct_pattern_dispatch(&nested, p);
                            }
                        }
                    }
                    // Wildcard / other — no dispatchable binding.
                    _ => {}
                },
            }
        }
    }

    /// Whether a destructured field's type owns heap that this slice's
    /// per-field cleanup handles: Vec/String, Map, `Set` (lowers to
    /// `Map[T, ()]`), or a non-shared user struct. Nested-enum and
    /// nested-pattern fields remain out of scope (a narrow remaining leak,
    /// never a double-free).
    fn destructure_field_needs_cleanup(&self, te: &TypeExpr) -> bool {
        if self.extract_vec_elem_type(te).is_some()
            || self.is_string_type_expr(te)
            || self.extract_map_kv_types(te).is_some()
            || self.extract_set_elem_type(te).is_some()
            // B-2026-07-03-28 Facet A — an `Option[inline-heap]` leaf owns its
            // payload once destructured out of a callee-owned source (whose
            // struct-drop `OptionInline` free is suppressed by the tag-zero at
            // the move site). An unconsumed leaf must free the payload itself.
            || self.option_inline_payload_elem(te).is_some()
        {
            return true;
        }
        if let TypeKind::Path(p) = &te.kind {
            if let Some(seg) = p.segments.last() {
                return self.struct_types.contains_key(seg.as_str())
                    && !self.shared_types.contains_key(seg.as_str());
            }
        }
        false
    }

    /// #23 — derive the element `TypeExpr`s of a let-bound tuple, for the
    /// `TypeExpr`-driven drop registration of a tuple var whose only heap is an
    /// enum / Map / Set leaf (invisible to `aggregate_has_heap_field`). Prefers
    /// the explicit annotation (`let t: (Map[K,V], i64) = …`); else infers from
    /// the RHS tuple literal's elements via `infer_arg_elem_te` (enum-ctor /
    /// value type → single-segment `Path`). Returns `None` when neither source
    /// is a tuple shape (e.g. a call-result RHS with no annotation — that tail
    /// stays with #24), so the caller falls through to the LLVM-type path.
    fn tuple_binding_elem_tes(&self, ty: Option<&TypeExpr>, value: &Expr) -> Option<Vec<TypeExpr>> {
        if let Some(TypeExpr {
            kind: TypeKind::Tuple(elems),
            ..
        }) = ty
        {
            return Some(elems.clone());
        }
        if let ExprKind::Tuple(elems) = &value.kind {
            return Some(elems.iter().map(|e| self.infer_arg_elem_te(e)).collect());
        }
        // #24 (B-2026-06-14-2) — the call-result source with no annotation
        // (`let p = ret_tuple(i)` where `ret_tuple -> (Tok, i64)`). The RHS is a
        // `Call`, not a tuple
        // literal, so the two arms above miss it and the enum/Map/Set leaf
        // leaked (`track_tuple_var` is enum-blind). Recover the element TEs from
        // the callee's recorded return type (`fn_return_type_exprs`) — the same
        // free-function-Call recovery `untyped_let_boxed_enum_te` does for boxed
        // enums. A method-call RHS (`let p = obj.split()`) is the deferred narrow
        // tail (methods aren't keyed in `fn_return_type_exprs`); it leaks but
        // never double-frees, matching the boxed-enum spike's method-call defer.
        if let ExprKind::Call { callee, .. } = &value.kind {
            if let ExprKind::Identifier(name) = &callee.kind {
                if let Some(TypeExpr {
                    kind: TypeKind::Tuple(elems),
                    ..
                }) = self.fn_return_type_exprs.get(name)
                {
                    return Some(elems.clone());
                }
            }
        }
        None
    }

    /// Queue the right scope-exit `CleanupAction` for an owned destructured
    /// field given its slot + source `TypeExpr`. Mirrors the simple-`let`
    /// cleanup arms (`track_vec_var` / `track_map_var` / `track_struct_var`).
    /// The var must already be registered via `register_var_from_type_expr`
    /// (so the Map flag lookups resolve).
    fn track_owned_destructure_field_cleanup(
        &mut self,
        var_name: &str,
        alloca: PointerValue<'ctx>,
        te: &TypeExpr,
    ) {
        if let Some(elem_ty) = self.extract_vec_elem_type(te) {
            // B-2026-07-04-9(a) — a `Vec[<aggregate>]` leaf whose element owns
            // heap the outer-buffer free can't reach (a struct/enum/Option
            // element, e.g. `Vec[ArgN]`, `ArgN { name: Option[String] }`) must
            // drain each element via its own `__karac_drop_*`
            // (`vec_elem_agg_drop_for_type_expr`) — the SAME per-element drain the
            // whole-struct drop (`emit_struct_drop_synthesis`'s VecOrString arm)
            // and the for-loop iterable (`track_vec_of_aggs_var`) use. `track_vec_var`
            // alone frees only the outer `{ptr,len,cap}` buffer plus DIRECT
            // String/Vec element buffers, so when the entry-copy makes each
            // element's inner heap INDEPENDENT (B-04-9(a) element-deep copy) this
            // leaf strands those payloads (the consume-path leak that reverted two
            // prior attempts). Symmetric with that deep copy: whatever the copy
            // duplicates, this drain frees. Direct String/Vec/Map/Set elements
            // return `None` here (their buffers are handled by `track_vec_var`
            // below) — unchanged.
            if let Some(agg_drop) = crate::codegen::helpers::vec_inner_type_expr(te)
                .and_then(|elem_te| self.vec_elem_agg_drop_for_type_expr(&elem_te))
            {
                self.track_vec_of_aggs_var(alloca, elem_ty, agg_drop);
                return;
            }
            self.track_vec_var(alloca, Some(elem_ty));
            return;
        }
        if self.is_string_type_expr(te) {
            let i8t = self.context.i8_type().into();
            self.track_vec_var(alloca, Some(i8t));
            return;
        }
        // B-2026-07-03-28 Facet A — an `Option[inline-heap]` destructure leaf.
        // Register it exactly like a simple `let sv: Option[String] = …` binding:
        // a scope-exit `FreeInlineOptionPayload` (tag-guarded) plus membership in
        // `inline_option_payload_vars`, so a later `match sv` / move of the leaf
        // suppresses the free (no double-drop) and an unconsumed leaf frees the
        // payload itself (no leak).
        if self.option_inline_payload_elem(te).is_some() {
            self.track_inline_option_payload_var(var_name, alloca, te);
            return;
        }
        if self.extract_map_kv_types(te).is_some() || self.extract_set_elem_type(te).is_some() {
            // Map and Set share one cleanup (Set lowers to `Map[T, ()]`):
            // `key_is_vec` falls back to `set_elem_types`, and a Set has no
            // value half so `val_is_vec` / `val_shared_heap` are naturally
            // empty (`map_val_types` never holds a Set var). Mirrors the
            // simple-`let` Map/Set cleanup arm.
            let key_is_vec = self
                .map_key_types
                .get(var_name)
                .or_else(|| self.set_elem_types.get(var_name))
                .copied()
                .is_some_and(|t| self.llvm_ty_is_vec_struct(t));
            let val_is_vec = self
                .map_val_types
                .get(var_name)
                .copied()
                .is_some_and(|t| self.llvm_ty_is_vec_struct(t));
            let val_shared_heap = self.map_val_shared_heap_type_for(var_name);
            let key_shared_heap = self.map_key_shared_heap_type_for(var_name);
            // Slice 3r: per-value drop fn for non-overlay heap values.
            let val_drop_fn = self
                .var_elem_type_exprs
                .get(var_name)
                .cloned()
                .and_then(|vte| self.map_val_drop_fn_for_type_expr(&vte));
            let (val_is_vec, val_shared_heap) = if val_drop_fn.is_some() {
                (false, None)
            } else {
                (val_is_vec, val_shared_heap)
            };
            self.track_map_var_with_val_drop(
                alloca,
                key_is_vec,
                val_is_vec,
                val_shared_heap,
                key_shared_heap,
                val_drop_fn,
            );
            return;
        }
        if let TypeKind::Path(p) = &te.kind {
            if let Some(seg) = p.segments.last() {
                if self.struct_types.contains_key(seg.as_str())
                    && !self.shared_types.contains_key(seg.as_str())
                {
                    self.track_struct_var(seg, alloca);
                }
            }
        }
    }

    // ── Slice c-repl.B.5.1: REPL value-snapshot helpers ─────────────────
    //
    // Routes top-level `let <name> = <expr>` lets through a per-binding
    // LLVM global as a cross-cell side channel. Cell N (capture) defines
    // `@__karac_repl_snapshot_<name>` with External linkage and stores
    // the bound value into it just after `bind_pattern`. Cell N+1
    // (replay) declares the same symbol as external and emits a load
    // from it, skipping the original RHS. The JIT linker resolves both
    // references through the shared JITDylib's symbol table — the
    // mechanism mirrors slice B.4's function declare-only path but for
    // data, not code.
    //
    // Caveats baked into the design:
    //   * Single-binding `PatternKind::Binding` only. Destructuring lets
    //     (`let (a, b) = …`, struct patterns, slice patterns) fall
    //     through to normal RHS evaluation — keyed on a single name
    //     doesn't cover them, mirrors the interpreter's snapshot
    //     classification (`parse_let_binding_names`).
    //   * Primitive types only (i64, f64, bool, char). String /
    //     aggregates would need a per-type stash format + ownership
    //     handshake; deferred to follow-on slices.

    /// Short-circuit `compile_stmt` when this stmt is a top-level
    /// `let <name> = <expr>` whose binding name is in
    /// `snapshot_replay`. Emits a load from the cell-spanning global
    /// `__karac_repl_snapshot_<name>` into the binding's slot and
    /// returns `Ok(true)`, instructing the caller to skip the rest of
    /// the per-stmt dispatch (including the original RHS lowering).
    /// Returns `Ok(false)` for every non-replay stmt; non-REPL builds
    /// always take the false arm because `snapshot_replay` is empty.
    fn try_compile_snapshot_replay(&mut self, stmt: &Stmt) -> Result<bool, String> {
        let StmtKind::Let { pattern, .. } = &stmt.kind else {
            return Ok(false);
        };
        let PatternKind::Binding(name) = &pattern.kind else {
            return Ok(false);
        };
        let Some(&kind) = self.snapshot_replay.get(name) else {
            return Ok(false);
        };

        let global = self.get_or_declare_snapshot_global(name, kind);
        let llvm_ty = self.snapshot_storage_type(kind);
        let loaded = self
            .builder
            .build_load(
                llvm_ty,
                global.as_pointer_value(),
                &format!("snap_load_{name}"),
            )
            .map_err(|e| format!("snapshot load: {e}"))?;
        let bound_val = self.snapshot_storage_to_binding(kind, loaded)?;
        self.bind_pattern(pattern, bound_val)?;
        // Slice c-repl.B.5.2: String replay needs the same dispatch-
        // map entries the normal let-arm String detection sets
        // (`vec_elem_types[name] = i8` so the slot is recognized as
        // a `{ptr, i64, i64}` Vec/String layout for GEPs, and
        // `string_vars.insert(name)` so method calls like `s.len()`,
        // `s.push_str(…)`, `println(s)` resolve through the String
        // dispatch surface). The replay path short-circuits at the
        // top of `compile_stmt`, so the let-arm's String detection
        // never runs for replayed bindings — without these
        // registrations, subsequent ops fall through to "unknown
        // type" handlers and crash. NO `track_vec_var` though: the
        // buffer is owned by the snapshot global, not this slot;
        // scope-exit cleanup must skip the free entirely.
        if matches!(kind, super::SnapshotPrimKind::String) {
            self.vec_elem_types
                .insert(name.clone(), self.context.i8_type().into());
            self.string_vars.insert(name.clone());
        }
        // Slice c-repl.B.5.3: Vec replay registers the binding under
        // `vec_elem_types[name]` with the actual element LLVM type so
        // downstream method dispatch (`xs.len()`, `xs.push(…)`,
        // `xs[i]`) routes through the Vec surface unchanged. Distinct
        // from the String arm above: NO `string_vars` insert (Vec is
        // not a String), and the elem type is per-variant rather than
        // the hardcoded i8 String layout. NO `track_vec_var` — the
        // buffer is owned by the snapshot global, not this slot's
        // alloca; scope-exit cleanup must skip the free.
        if let super::SnapshotPrimKind::Vec(elem) = kind {
            let elem_ty = self.vec_elem_llvm_type(elem);
            self.vec_elem_types.insert(name.clone(), elem_ty);
        }
        // Slice c-repl.B.5.3b: Map replay registers the binding under
        // `map_key_types[name]` / `map_val_types[name]` /
        // `map_key_type_names[name]` so downstream method dispatch
        // (`m.get(k)`, `m.insert(k, v)`, etc.) routes through the
        // Map surface unchanged. The key-name fallback through
        // `vec_elem_kind_name` matches what `extract_map_key_name`
        // would have produced from a `Map[K, V]` type annotation —
        // letting `emit_hash_fn_for_type` find the right primitive
        // hash/eq pair without needing a TypeExpr. No
        // `track_map_var`: the handle is owned by the snapshot
        // global, not this slot's alloca; scope-exit cleanup must
        // skip the free entirely.
        if let super::SnapshotPrimKind::Map { key, val } = kind {
            let key_ty = self.vec_elem_llvm_type(key);
            let val_ty = self.vec_elem_llvm_type(val);
            self.map_key_types.insert(name.clone(), key_ty);
            self.map_val_types.insert(name.clone(), val_ty);
            self.map_key_type_names
                .insert(name.clone(), Self::vec_elem_kind_name(key).to_string());
        }
        // Slice c-repl.B.5.3c: Set replay registers the binding under
        // `set_elem_types[name]` / `set_elem_type_names[name]` so
        // downstream method dispatch (`s.contains(x)`, `s.insert(x)`,
        // `s.len()`, etc.) routes through the Set surface unchanged.
        // Set's runtime is `karac_map_*` with val_size = 0, so the
        // handle in the snapshot global is layout-compatible with a
        // Map handle. No `track_map_var`: the handle is owned by the
        // snapshot global, not this slot's alloca; scope-exit cleanup
        // must skip the free entirely.
        if let super::SnapshotPrimKind::Set(elem) = kind {
            let elem_ty = self.vec_elem_llvm_type(elem);
            self.set_elem_types.insert(name.clone(), elem_ty);
            self.set_elem_type_names
                .insert(name.clone(), Self::vec_elem_kind_name(elem).to_string());
        }
        Ok(true)
    }

    /// Mirror of `try_compile_snapshot_replay` for the capture side.
    /// Called from the bottom of the `StmtKind::Let` arm just after
    /// `bind_pattern` has alloca'd + populated the binding's slot.
    /// Loads the slot's value, converts it to the storage form, and
    /// stores into `__karac_repl_snapshot_<name>`. No-op when the
    /// binding is not in `snapshot_capture` (every non-REPL build
    /// passes through here without effect because the map is empty).
    fn try_emit_snapshot_capture(&mut self, pattern: &Pattern) {
        let PatternKind::Binding(name) = &pattern.kind else {
            return;
        };
        let Some(&kind) = self.snapshot_capture.get(name) else {
            return;
        };
        let Some(slot) = self.variables.get(name).copied() else {
            return;
        };

        let global = self.get_or_define_snapshot_global(name, kind);
        let loaded =
            match self
                .builder
                .build_load(slot.ty, slot.ptr, &format!("snap_capture_{name}"))
            {
                Ok(v) => v,
                Err(_) => return,
            };
        let stored = match self.snapshot_binding_to_storage(kind, loaded) {
            Ok(v) => v,
            Err(_) => return,
        };
        let _ = self.builder.build_store(global.as_pointer_value(), stored);
        // Slice c-repl.B.5.2: String capture transfers buffer ownership
        // from the let slot to the global (option (a) "leak the
        // buffer" per the tracker entry). The slot's queued
        // `FreeVecBuffer` cleanup is suppressed by zeroing its cap —
        // `emit_scope_cleanup`'s walker treats `cap == 0` as
        // "nothing to free". The buffer survives until the JITDylib
        // is torn down (runner death / `:reset` / cross-cell shadow,
        // all of which drop the runner and reclaim its heap). No
        // suppression needed for primitive kinds — their globals
        // hold values, not pointers.
        if matches!(
            kind,
            super::SnapshotPrimKind::String | super::SnapshotPrimKind::Vec(_)
        ) {
            // Guard by the SLOT's actual LLVM type, not just `kind`. The
            // `snapshot_capture` map is keyed by NAME, so a cross-type
            // cross-cell rebind — `let x = 5` (replayed as an i64 binding)
            // followed by `let x: String = …` in the same synthesized cell —
            // makes `snapshot_capture["x"] == String` fire this branch for
            // BOTH bindings, including the i64 one whose slot is an 8-byte
            // `alloca i64`. `zero_vec_alloca_cap` GEPs field 2 (`cap`, offset
            // 16) of a `{ptr,i64,i64}`, so on the i64 slot it stores 8 bytes
            // 16 bytes past the alloca and corrupts the frame — tolerated
            // under AOT (frame slack) but the tighter LLJIT frame puts a live
            // pointer there and the cell crashes at PC=0 (B-2026-07-07-6).
            // Only a slot that actually holds the `{ptr,i64,i64}` struct
            // inline owns a `cap` to suppress; the i64 binding has nothing to
            // free and is skipped (its spurious capture-store is harmless —
            // the real String binding's capture overwrites the global).
            let holds_vec_struct = matches!(
                slot.ty,
                inkwell::types::BasicTypeEnum::StructType(held) if held == self.vec_struct_type()
            );
            if holds_vec_struct {
                self.zero_vec_alloca_cap(slot.ptr);
            }
        }
        // Slice c-repl.B.5.3b/B.5.3c: no slot-suppression for Map or
        // Set. Unlike Vec/String which use a cap=0 sentinel in the
        // slot's struct triple, Map/Set cleanup is queue-driven
        // (`FreeMapHandle` is pushed to `scope_cleanup_actions` from a
        // known set of sites). Suppression happens at the registration
        // site instead — `compile_map_new_stmt` / `compile_set_new_stmt`
        // skip `track_map_var` when
        // `snapshot_capture.contains_key(var_name)`. The slot keeps
        // the live handle so same-cell `m.insert(...)` / `m.get(...)`
        // / `s.insert(...)` / `s.contains(...)` still find the Map/Set;
        // no nulling required.
    }

    /// LLVM storage type for a snapshot global. Distinct from the
    /// binding-slot LLVM type for `Bool` (slot is i1, storage is i8)
    /// so the global's width is portable across cells that may load
    /// it through a different codegen invocation; every other kind
    /// uses the same width as the slot. `String` uses the standard
    /// `{ i8*, i64, i64 }` (ptr, len, cap) layout that `vec_struct_type`
    /// produces — same struct shape both `let` slots and the
    /// snapshot global use, so the load/store handshake doesn't need
    /// a conversion step.
    fn snapshot_storage_type(&self, kind: super::SnapshotPrimKind) -> BasicTypeEnum<'ctx> {
        match kind {
            super::SnapshotPrimKind::I64 => self.context.i64_type().into(),
            super::SnapshotPrimKind::F64 => self.context.f64_type().into(),
            super::SnapshotPrimKind::Bool => self.context.i8_type().into(),
            super::SnapshotPrimKind::Char => self.context.i32_type().into(),
            super::SnapshotPrimKind::String | super::SnapshotPrimKind::Vec(_) => {
                self.vec_struct_type().into()
            }
            super::SnapshotPrimKind::Map { .. } | super::SnapshotPrimKind::Set(_) => {
                self.context.ptr_type(AddressSpace::default()).into()
            }
        }
    }

    /// Slice c-repl.B.5.3: LLVM type for a Vec elem variant. The replay
    /// path uses this to register `vec_elem_types[name]` with the right
    /// per-element width so downstream method/index dispatch finds the
    /// binding through the existing Vec surface.
    fn vec_elem_llvm_type(&self, elem: super::VecElemKind) -> BasicTypeEnum<'ctx> {
        match elem {
            super::VecElemKind::I64 => self.context.i64_type().into(),
            super::VecElemKind::F64 => self.context.f64_type().into(),
            super::VecElemKind::Bool => self.context.bool_type().into(),
            super::VecElemKind::Char => self.context.i32_type().into(),
        }
    }

    /// Slice c-repl.B.5.3b: name string for a Vec elem variant — the
    /// same mangled name `extract_map_key_name` produces for the same
    /// primitive type. `map_key_type_names[name]` falls back through
    /// this when reconstructing Map hash/eq dispatch for a snapshot-
    /// replayed binding.
    fn vec_elem_kind_name(elem: super::VecElemKind) -> &'static str {
        match elem {
            super::VecElemKind::I64 => "i64",
            super::VecElemKind::F64 => "f64",
            super::VecElemKind::Bool => "bool",
            super::VecElemKind::Char => "char",
        }
    }

    /// Convert a value loaded from the snapshot global into the LLVM
    /// type the binding's slot expects. Identity for i64/f64/char;
    /// i8 → i1 for Bool.
    fn snapshot_storage_to_binding(
        &self,
        kind: super::SnapshotPrimKind,
        loaded: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        match kind {
            super::SnapshotPrimKind::I64
            | super::SnapshotPrimKind::F64
            | super::SnapshotPrimKind::Char
            | super::SnapshotPrimKind::String
            | super::SnapshotPrimKind::Vec(_)
            | super::SnapshotPrimKind::Map { .. }
            | super::SnapshotPrimKind::Set(_) => Ok(loaded),
            super::SnapshotPrimKind::Bool => {
                let i8_val = loaded.into_int_value();
                let zero = self.context.i8_type().const_zero();
                let i1 = self
                    .builder
                    .build_int_compare(IntPredicate::NE, i8_val, zero, "snap_to_i1")
                    .map_err(|e| format!("snapshot bool->i1: {e}"))?;
                Ok(i1.as_basic_value_enum())
            }
        }
    }

    /// Inverse of `snapshot_storage_to_binding`: convert a slot-loaded
    /// value into the storage representation. Bool gets z-extended
    /// i1 → i8; every other kind passes through.
    fn snapshot_binding_to_storage(
        &self,
        kind: super::SnapshotPrimKind,
        loaded: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        match kind {
            super::SnapshotPrimKind::I64
            | super::SnapshotPrimKind::F64
            | super::SnapshotPrimKind::Char
            | super::SnapshotPrimKind::String
            | super::SnapshotPrimKind::Vec(_)
            | super::SnapshotPrimKind::Map { .. }
            | super::SnapshotPrimKind::Set(_) => Ok(loaded),
            super::SnapshotPrimKind::Bool => {
                let i1 = loaded.into_int_value();
                let i8_ty = self.context.i8_type();
                let extended = self
                    .builder
                    .build_int_z_extend(i1, i8_ty, "snap_bool_to_i8")
                    .map_err(|e| format!("snapshot bool->i8: {e}"))?;
                Ok(extended.as_basic_value_enum())
            }
        }
    }

    /// Get-or-declare the externally-visible `__karac_repl_snapshot_<name>`
    /// global for replay. Returns the existing global if this cell's
    /// codegen pass has already touched it; otherwise emits a body-
    /// less external declaration so the JIT linker can resolve the
    /// reference against the defining cell's module in the same
    /// JITDylib.
    fn get_or_declare_snapshot_global(
        &self,
        name: &str,
        kind: super::SnapshotPrimKind,
    ) -> GlobalValue<'ctx> {
        let sym = format!("__karac_repl_snapshot_{name}");
        if let Some(g) = self.module.get_global(&sym) {
            return g;
        }
        let ty = self.snapshot_storage_type(kind);
        let g = self.module.add_global(ty, None, &sym);
        g.set_linkage(Linkage::External);
        g
    }

    /// Get-or-define the externally-visible snapshot global for
    /// capture. Mirror of the declare path but with a zero
    /// initializer (which lets LLVM treat the symbol as defined
    /// here — the JIT installs the slot into the JITDylib so
    /// future cells' external declarations can resolve to it).
    fn get_or_define_snapshot_global(
        &self,
        name: &str,
        kind: super::SnapshotPrimKind,
    ) -> GlobalValue<'ctx> {
        let sym = format!("__karac_repl_snapshot_{name}");
        if let Some(g) = self.module.get_global(&sym) {
            return g;
        }
        let ty = self.snapshot_storage_type(kind);
        let g = self.module.add_global(ty, None, &sym);
        match kind {
            super::SnapshotPrimKind::I64 => {
                g.set_initializer(&self.context.i64_type().const_zero());
            }
            super::SnapshotPrimKind::F64 => {
                g.set_initializer(&self.context.f64_type().const_zero());
            }
            super::SnapshotPrimKind::Bool => {
                g.set_initializer(&self.context.i8_type().const_zero());
            }
            super::SnapshotPrimKind::Char => {
                g.set_initializer(&self.context.i32_type().const_zero());
            }
            super::SnapshotPrimKind::String | super::SnapshotPrimKind::Vec(_) => {
                // Slice c-repl.B.5.2/B.5.3: zero-initialize the
                // (ptr, len, cap) triple. cap = 0 is the sentinel that
                // `FreeVecBuffer` checks before freeing, so an
                // uncaptured global (no cell has executed the capture
                // path yet) won't free anything if accidentally
                // treated as a String/Vec slot.
                g.set_initializer(&self.vec_struct_type().const_zero());
            }
            super::SnapshotPrimKind::Map { .. } | super::SnapshotPrimKind::Set(_) => {
                // Slice c-repl.B.5.3b/B.5.3c: zero-initialize the
                // handle pointer. `karac_map_free` early-returns on a
                // null map, so an uncaptured global accidentally
                // treated as a Map/Set slot is a safe no-op. Set
                // lowers to `Map[T, ()]` so the same handle layout
                // and same `karac_map_free` cleanup apply.
                g.set_initializer(&self.context.ptr_type(AddressSpace::default()).const_null());
            }
        }
        g.set_linkage(Linkage::External);
        g
    }
}

impl<'ctx> super::Codegen<'ctx> {
    /// Detect statements that discard the result of a direct method call:
    /// `let _ = obj.method(...)` (Wildcard let-binding) or a bare
    /// `obj.method(...);` expression statement. Returns the receiver
    /// identifier name and the method name when matched, so the caller
    /// can gate further work on the specific receiver type. Returns
    /// `None` for any other shape — nested calls, non-Identifier
    /// receivers, non-Wildcard let patterns, etc. Used by the
    /// `Map.insert` shared-value overwrite-leak fix; safe to extend
    /// to other discard-leak shapes by checking the returned method
    /// name at the call site.
    pub(super) fn stmt_discards_method_call(stmt: &Stmt) -> Option<(&str, &str)> {
        let expr: &Expr = match &stmt.kind {
            StmtKind::Let { pattern, value, .. }
                if matches!(&pattern.kind, PatternKind::Wildcard) =>
            {
                value
            }
            StmtKind::Expr(e) => e,
            _ => return None,
        };
        if let ExprKind::MethodCall { object, method, .. } = &expr.kind {
            if let ExprKind::Identifier(name) = &object.kind {
                return Some((name.as_str(), method.as_str()));
            }
        }
        None
    }

    /// B-2026-06-17-2 — is `stmt` a **discarded** `spawn(...)` / `tg.spawn(...)`?
    /// True for a bare `spawn(c);` / `obj.spawn(c);` expression-statement or a
    /// `let _ = spawn(c)` / `let _ = obj.spawn(c)`, i.e. the spawn call is the
    /// statement's head expression so its result `TaskHandle` is the value the
    /// statement throws away (never bound, never `.join()`ed). Drives
    /// `pending_spawn_detach`, which `lower_spawn_shared` consumes to emit a
    /// `karac_runtime_task_detach` so the runtime eager-reaps the handle.
    ///
    /// Syntactic by design: the flag is only ever *consumed* when a genuine
    /// `spawn` / `TaskGroup.spawn` lowering runs, so an `obj.spawn(...)` on some
    /// unrelated type never reaches the detach emission, and the flag is reset
    /// at the next statement regardless. Restricting to the *head* expression
    /// (not arbitrary nested calls) is what keeps a `process(spawn(|| …))` —
    /// where the handle is handed to `process`, not discarded — from being
    /// wrongly detached.
    pub(super) fn stmt_is_discarded_spawn(stmt: &Stmt) -> bool {
        let expr: &Expr = match &stmt.kind {
            StmtKind::Let { pattern, value, .. }
                if matches!(&pattern.kind, PatternKind::Wildcard) =>
            {
                value
            }
            StmtKind::Expr(e) => e,
            _ => return false,
        };
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                args.len() == 1 && matches!(&callee.kind, ExprKind::Identifier(n) if n == "spawn")
            }
            ExprKind::MethodCall { method, args, .. } => method == "spawn" && args.len() == 1,
            _ => false,
        }
    }

    /// General owned-temp tracking discard gate (see
    /// `docs/spikes/general-owned-temp-tracking.md`): does `expr`, in
    /// statement-discard position, produce a *fresh-owned* value whose heap
    /// storage this scope must drop? Restricted to `Call` / `MethodCall` —
    /// the dominant fresh-temp sources, and the only shapes guaranteed not to
    /// alias an existing tracked binding. A `Call`/`MethodCall` returning an
    /// *owned* `Vec[T]` / `String` / `Map`/`Set` handle / shared-struct RC box
    /// transfers fresh storage to the caller (callee move-out);
    /// `materialize_owned_temp` then classifies the kind (Vec/String by LLVM
    /// type, Map/RC via the `owned_temp_drops` hint table). *Place* expressions
    /// (`Identifier` / field / index) are deliberately excluded: their value
    /// reloads an existing binding's storage, which a second free/dec would
    /// double-free. Conservative by design — when unsure, leak (safe) rather
    /// than double-free (UB). Discarded literals / operator results
    /// (`[1, 2, 3];`, `"a" + "b";`) are rare and left to a later slice.
    ///
    /// **Borrow-returning free-fn calls are excluded** (`name_of(s)` where
    /// `name_of(_) -> ref T`): their result aliases the borrow source, not a
    /// fresh allocation, so freeing it double-frees the source. The original
    /// design relied on a borrow callee yielding a bare `ptr` value (no
    /// `owned_temp_drops` entry → auto-excluded), but a *direct-use* consumer
    /// (`name_of(s).len()`, `match name_of(s) { … }`) first routes the call
    /// through `compile_call`'s value-position relaxation, which LOADS the
    /// pointee into a `{ptr,len,cap}` struct — defeating the ptr-shape
    /// auto-exclusion and re-classifying it as an owned String/Vec
    /// (B-2026-06-10-5). So we exclude it explicitly here. (Borrow-returning
    /// *method* receivers used directly — `u.name().len()` — are rejected
    /// upstream by the `user_ref_method_names` gate in `compile_method_call`,
    /// so the free-fn check suffices.)
    pub(super) fn expr_yields_fresh_owned_temp(&self, expr: &Expr) -> bool {
        matches!(
            &expr.kind,
            ExprKind::Call { .. } | ExprKind::MethodCall { .. }
        ) && !self.is_borrow_returning_call_expr(expr)
    }

    /// True if `expr` is a `String[a..b]` / `String[a..=b]` range-index slice
    /// over a string-typed object — which `compile_index` → `compile_string_slice`
    /// lowers to a *freshly* `karac_string_slice`-allocated owned `{ptr,len,cap=N}`
    /// temp (cap > 0), exactly like a `s.substring(a, b)` call. A range slice is
    /// not a `Call`/`MethodCall`, so `expr_yields_fresh_owned_temp` misses it; but
    /// in the same copy-consuming borrow contexts (`push_str`, `contains`,
    /// `starts_with`) the freshly-allocated slice buffer is the caller's to free,
    /// and without it leaks once per call — unbounded in a loop (B-2026-06-12-5:
    /// `buffer.push_str(src[a..b])`, the lexer's zero-copy token-text shape passed
    /// to a copying sink). `string_typed_exprs` membership of the *object* is the
    /// same gate `try_compile_borrowed_string_slice` / `compile_index` use to route
    /// String slicing, so a `Vec[T]` index (`v[i]`, a place/element copy) is never
    /// matched here. The `cap > 0` guard at the free site is the backstop: were
    /// this ever lowered to the borrowed (cap == 0) view, the free no-ops.
    pub(super) fn expr_is_fresh_owned_string_slice(&self, expr: &Expr) -> bool {
        if let ExprKind::Index { object, index } = &expr.kind {
            if matches!(&index.kind, ExprKind::Range { .. }) {
                return self
                    .string_typed_exprs
                    .contains(&(object.span.offset, object.span.length));
            }
        }
        false
    }

    /// General owned-temp tracking, slice 5 (see
    /// `docs/spikes/general-owned-temp-tracking.md`): peel single-tail block
    /// wrappers (`{ … make() }`, `unsafe { … }`, a labeled block) down to the
    /// tail expression a *discarded* value actually originates from, so a
    /// fresh owned temp produced in a block tail position — `{ make() }` in
    /// statement position, or `let _ = { make() };` — routes through the
    /// owned-temp chokepoint at the discard site instead of leaking. Returns
    /// the tail `Expr` (whose span keys the `owned_temp_drops` hint table)
    /// iff it yields a fresh owned temp; `None` leaves the value untracked,
    /// exactly as before.
    ///
    /// Only *single-tail* wrappers are peeled. Branching tails (`if` / `match`
    /// in tail position) are deliberately excluded: a branch whose tail is a
    /// *place* expression (an aliased binding) would be double-freed against
    /// its own cleanup, so discarded branching tails stay a (safe) leak for a
    /// later slice. Phi-merged fresh-temp branches are the only thing lost by
    /// this conservatism, and they are rare in discard position.
    pub(super) fn discarded_owned_temp_tail(expr: &Expr) -> Option<&Expr> {
        match &expr.kind {
            ExprKind::Call { .. } | ExprKind::MethodCall { .. } => Some(expr),
            ExprKind::Block(block)
            | ExprKind::Seq(block)
            | ExprKind::Unsafe(block)
            | ExprKind::LabeledBlock { body: block, .. } => block
                .final_expr
                .as_deref()
                .and_then(Self::discarded_owned_temp_tail),
            _ => None,
        }
    }

    /// Phase-8 line 39 follow-up — does `expr` evaluate to a live
    /// (un-`send()`-ed) `RequestBuilder` value minted by a `.request(...)`
    /// chain? A chained builder produced as a *discarded* statement
    /// (`c.request(url).header(...);` with no `.send()` and no binding) is
    /// a temporary, and Kāra has no general temporary-drop, so its
    /// runtime `HTTP_BUILDERS` entry would leak until process exit. When
    /// this returns true the `StmtKind::Expr` / wildcard-`let _` arms free
    /// the handle off the discarded value via
    /// `karac_runtime_http_builder_free`.
    ///
    /// A chain ending in `.send()` yields a `Result` (and the runtime
    /// already removed the entry), so it returns false. An `Identifier`
    /// root (a let-bound builder) is excluded — those are drop-tracked by
    /// their own `StructDrop`; only the unbound `.request(...)`-rooted
    /// method chain is a leaking temporary.
    pub(super) fn expr_is_live_request_builder_temp(expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::MethodCall { object, method, .. } => match method.as_str() {
                // `Client.request(method, url)` mints a fresh builder.
                "request" => true,
                // The owned-self setters return the same live builder.
                "header" | "body" | "timeout" => Self::expr_is_live_request_builder_temp(object),
                // `send` consumes it into a `Result`; anything else isn't
                // part of a builder chain.
                _ => false,
            },
            _ => false,
        }
    }

    /// Free the abandoned-handle of a discarded live `RequestBuilder`
    /// temporary (the value `compile_expr` produced for `discarded_expr`).
    /// No-op unless `discarded_expr` is a `.request(...)`-rooted chain
    /// that wasn't `.send()`-ed (see `expr_is_live_request_builder_temp`).
    pub(super) fn free_discarded_request_builder_temp(
        &self,
        discarded_expr: &Expr,
        val: inkwell::values::BasicValueEnum<'ctx>,
    ) {
        if !Self::expr_is_live_request_builder_temp(discarded_expr) {
            return;
        }
        let BasicValueEnum::StructValue(sv) = val else {
            return;
        };
        let Ok(handle) = self.builder.build_extract_value(sv, 0, "rb.abandon.handle") else {
            return;
        };
        let free_fn = self
            .module
            .get_function("karac_runtime_http_builder_free")
            .expect("karac_runtime_http_builder_free declared in Codegen::new");
        let _ = self
            .builder
            .build_call(free_fn, &[handle.into_int_value().into()], "");
    }
}

impl<'ctx> super::Codegen<'ctx> {
    /// Recurse into branching tail expressions to decide whether the RHS of a
    /// shared-type `let` / `Assign` already delivers a freshly-owned ref
    /// (callee move-out for `Call` / `MethodCall`, or `emit_rc_alloc` for
    /// `StructLiteral`). The receive-site `rc_inc` must be skipped exactly
    /// when this returns `true`; otherwise the refcount lands at 2 for a
    /// genuinely fresh value and leaks one ref per crossing (same shape as
    /// bug #8 receive-side, but for `If` / `Match` / `IfLet` / `Block` /
    /// `LabeledBlock` / `Unsafe` tails that nest the fresh-ref source one
    /// level deeper than the outer `ExprKind` reveals).
    ///
    /// `unwrap()` / `expect()` on an Option/Result receiver are the
    /// deliberate exception to the "MethodCall ⇒ fresh" rule: their
    /// lowering (`try_compile_option_result_method`) only re-extracts the
    /// receiver aggregate's payload words — for a `shared T` payload
    /// that's a borrowing alias with NO +1 transfer. Classifying them as
    /// fresh skipped the receive-inc while `track_rc_var` still queued the
    /// scope-exit dec, so each `let node = cur.unwrap();` over-dec'd the
    /// chain by one (the list-walk kata shape freed the list out from
    /// under its own cursor). Discriminated via the typechecker-populated
    /// `method_unwrap_inner_types` side-table (keyed by the MethodCall
    /// span — the same key `try_compile_option_result_method` reads) so a
    /// user-defined `.unwrap()` on a non-Option/Result type keeps callee
    /// move-out semantics.
    ///
    /// Conservative on mixed-shape branches: returns `false` when ANY branch
    /// tail aliases an existing ref (`Identifier` / `FieldAccess` / `Index`
    /// / etc.), so the receive site still incs. The fresh-tail branches in
    /// that mix will double-inc (leaking +1 on those paths) — same behavior
    /// as before this helper — but the aliasing branch is preserved
    /// correctly. Per-branch inc emission would require lowering the
    /// receive-inc into each tail block; deferred to a future slice.
    pub(super) fn rhs_yields_fresh_ref(&self, expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::MethodCall { method, .. }
                if matches!(method.as_str(), "unwrap" | "expect")
                    && self
                        .method_unwrap_inner_types
                        .contains_key(&(expr.span.offset, expr.span.length)) =>
            {
                false
            }
            ExprKind::StructLiteral { .. }
            | ExprKind::Call { .. }
            | ExprKind::MethodCall { .. } => true,
            ExprKind::Block(block)
            | ExprKind::Unsafe(block)
            | ExprKind::LabeledBlock { body: block, .. } => block
                .final_expr
                .as_deref()
                .is_some_and(|e| self.rhs_yields_fresh_ref(e)),
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
                    .is_some_and(|e| self.rhs_yields_fresh_ref(e))
                    && else_branch
                        .as_deref()
                        .is_some_and(|e| self.rhs_yields_fresh_ref(e))
            }
            ExprKind::Match { arms, .. } => {
                !arms.is_empty() && arms.iter().all(|arm| self.rhs_yields_fresh_ref(&arm.body))
            }
            _ => false,
        }
    }
}
