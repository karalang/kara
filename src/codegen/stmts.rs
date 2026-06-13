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
                                self.track_vec_var(alloca, elem_ty);
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

        // Per-binding metadata: which branch defines it, and what's the
        // statement reference for type inference.
        let mut defined: HashMap<String, (usize, &Stmt)> = HashMap::new();
        for (branch_idx, &stmt_idx) in sorted_indices.iter().enumerate() {
            if stmt_idx >= body.stmts.len() {
                continue;
            }
            let stmt = &body.stmts[stmt_idx];
            match &stmt.kind {
                StmtKind::Let { pattern, .. } | StmtKind::LetElse { pattern, .. } => {
                    if let PatternKind::Binding(name) = &pattern.kind {
                        defined.insert(name.clone(), (branch_idx, stmt));
                    }
                }
                StmtKind::LetUninit { name: _, .. } => {
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
            if let Some(llvm_ty) = self.infer_let_binding_llvm_type(stmt) {
                slots.push(ReturnSlot {
                    binding_name: name,
                    branch_index: branch_idx,
                    llvm_ty,
                });
            } else {
                // RHS shape we can't recover the LLVM type from. Bail
                // — the caller drops the par-group and falls back to
                // sequential compilation. Emitting the group with the
                // binding silently absent leaves it as a class-(i)
                // branch-local alloca and every later read site
                // fails with "Undefined variable".
                return None;
            }
        }
        Some(slots)
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
        let (ty_ann, value): (Option<&TypeExpr>, &Expr) = match &stmt.kind {
            StmtKind::Let { ty, value, .. } | StmtKind::LetElse { ty, value, .. } => {
                (ty.as_ref(), value)
            }
            _ => return None,
        };
        if let Some(te) = ty_ann {
            return Some(self.llvm_type_for_type_expr(te));
        }
        // Fallback: free-function call — read the declared return type
        // from the LLVM function declaration the parser/declare-pass
        // already minted.
        if let ExprKind::Call { callee, .. } = &value.kind {
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
        }
        // Fallback: alias of an in-scope variable — `let n = p` where `p`
        // is a param or earlier local. Read the type directly from the
        // variables table. Without this, auto-parallelization treats `n`
        // as un-typeable, drops it from the return-slot list, and the
        // tail-expression reference fails with "Undefined variable 'n'"
        // because the par-branch's local alloca never propagates to the
        // parent scope.
        if let ExprKind::Identifier(name) = &value.kind {
            if let Some(slot) = self.variables.get(name) {
                return Some(slot.ty);
            }
        }
        // Integer / bool literals carry their type directly. Sized
        // integer suffixes (`0i32`, `5u8`, …) map through
        // `const_int_for_suffix`'s sizing rules; the unsuffixed default
        // is `i64`, matching `const_int_for_suffix`. Same for floats.
        match &value.kind {
            ExprKind::Integer(_, sfx) => Some(self.const_int_for_suffix(0, *sfx).get_type().into()),
            ExprKind::Float(_, sfx) => {
                Some(self.const_float_for_suffix(0.0, *sfx).get_type().into())
            }
            ExprKind::Bool(_) => Some(self.context.bool_type().into()),
            _ => None,
        }
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
        let is_param_field = matches!(
            &value.kind,
            ExprKind::FieldAccess { object, .. }
                if matches!(&object.kind, ExprKind::Identifier(p)
                    if self.owned_struct_params.contains(p.as_str()))
        );
        if !is_param_field {
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
                            } else if surface == "String" {
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
                                self.vec_elem_types
                                    .insert(var_name.clone(), self.context.i8_type().into());
                                self.string_vars.insert(var_name.clone());
                                detected = true;
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
                // SoA layout: if variable matches a layout name and RHS is Vec::new(),
                // produce the SoA struct type instead of the normal Vec.
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    if let Some(soa) = self.soa_layouts.get(var_name.as_str()).cloned() {
                        if self.is_vec_new_call(value) {
                            return self.compile_soa_new(var_name, &soa);
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
                // Sibling threading for `Tensor.zeros/ones/full` in the
                // RHS — those constructors can't recover the element
                // type or rank from their `dims: Vec[i64]` argument;
                // `tensor_var_infos[var_name]` was populated above from
                // the annotation. Cleared after compile.
                let saved_pending_let_tensor = self.pending_let_tensor_info.take();
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    if let Some(&elem_ty) = self.vec_elem_types.get(var_name.as_str()) {
                        self.pending_let_elem_type = Some(elem_ty);
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
                }
                let val = self.compile_expr(value)?;
                self.pending_let_elem_type = saved_pending_let_elem;
                self.pending_let_tensor_info = saved_pending_let_tensor;
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
                let val = if rhs_is_owned_param {
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
                                        match self.struct_types.get(target) {
                                            Some(target_st) if *target_st == st => {
                                                Some(target.clone())
                                            }
                                            _ => None,
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
                                .cloned(),
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
                    self.bind_pattern(pattern, val)?;
                    // `let Point { items, count } = …` — `bind_pattern` only
                    // allocas the field bindings; it registers neither method
                    // dispatch nor scope-exit cleanup for them, so destructured
                    // heap fields used to be undispatchable (`items.len()` →
                    // "no handler for method") AND leaked. Wire both here (B
                    // follow-up #3 / docs/spikes/pattern-arm-unbound-field-drop.md).
                    if matches!(&pattern.kind, PatternKind::Struct { .. }) {
                        self.finish_owned_struct_destructure(pattern, value, val)?;
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
                            if !matches!(slot_ty, BasicTypeEnum::ArrayType(_)) {
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
                                if is_tensor_elem {
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
                        // the defensive-copy shim).
                        if !rhs_is_owned_param {
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
                        if matches!(&value.kind, ExprKind::Identifier(_)) {
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
                        }
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
                                if agg_ty != self.vec_struct_type()
                                    && self.aggregate_has_heap_field(agg_ty)
                                {
                                    self.track_tuple_var(slot.ptr, agg_ty);
                                    // `let u = t` tuple-to-tuple move: both slots
                                    // alias the same buffers; zero the source's
                                    // field caps so its drop no-ops and `u` owns
                                    // (no-op for a fresh tuple-literal RHS, which
                                    // isn't an Identifier).
                                    self.suppress_source_vec_cleanup_for_arg(value);
                                }
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
                        }
                    }
                }
                // Track Map/Set variables when the RHS is a fresh-handle-producing
                // method call (`clone`, `union`, `intersection`, `difference`).
                // `Map.new()` / `Set.new()` / map-literal RHS shapes already track
                // via their early-return paths above; `let n = m;` (move) bypasses
                // this since it's an Identifier RHS, not a MethodCall, so the
                // source's existing track stays the unique cleanup owner.
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    let fresh_handle = matches!(
                        &value.kind,
                        ExprKind::MethodCall { method, .. }
                            if matches!(
                                method.as_str(),
                                "clone" | "union" | "intersection" | "difference"
                            )
                    );
                    if fresh_handle
                        && (self.map_key_types.contains_key(var_name.as_str())
                            || self.set_elem_types.contains_key(var_name.as_str()))
                    {
                        if let Some(slot) = self.variables.get(var_name.as_str()) {
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
                            self.track_map_var(
                                slot.ptr,
                                key_is_vec,
                                val_is_vec,
                                val_shared_heap,
                                key_shared_heap,
                            );
                        }
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
                    if !handled_option && !handled_result && !handled_option_map {
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
                    self.compile_field_store(object, field, val, rhs_is_fresh)?;
                } else if let ExprKind::Index { object, index } = &target.kind {
                    self.compile_index_store(object, index, val)?;
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
                    if let Some(slot) = self.variables.get(name).copied() {
                        self.builder.build_store(slot.ptr, result).unwrap();
                    }
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
                    }
                }
                Ok(())
            }
            _ => Ok(()),
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
            self.track_vec_var(alloca, Some(elem_ty));
            return;
        }
        if self.is_string_type_expr(te) {
            let i8t = self.context.i8_type().into();
            self.track_vec_var(alloca, Some(i8t));
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
            self.track_map_var(
                alloca,
                key_is_vec,
                val_is_vec,
                val_shared_heap,
                key_shared_heap,
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
            self.zero_vec_alloca_cap(slot.ptr);
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
    /// same gate `try_compile_borrowed_string_key` / `compile_index` use to route
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
