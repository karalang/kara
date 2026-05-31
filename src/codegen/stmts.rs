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
use inkwell::values::{BasicValue, BasicValueEnum, GlobalValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

use super::helpers::{
    map_kv_type_exprs, set_inner_type_expr, slice_inner_type_expr, vec_inner_type_expr,
};
use super::state::{ReturnSlot, SharedTypeInfo, VarSlot};

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
    /// - `Some(...)` / a call move-out / `var.field` / anything else get no inc
    ///   (the constructor already inc'd a `Some` payload; a call owns its ref;
    ///   `var.field` is handled by `suppress_tail_field_option_dec`).
    pub(super) fn compile_tail_final_expr(
        &mut self,
        expr: &Expr,
        tail_inner: Option<StructType<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
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
            self.drain_top_frame_with_emit();
        } else {
            self.scope_cleanup_actions.pop();
        }
        Ok(result)
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
                let (slot_values, _) =
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
            StmtKind::Let {
                pattern, value, ty, ..
            } => {
                // Track Vec/String element types from type annotation or RHS.
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    let mut detected = false;
                    // Explicit type annotation: let v: Vec[T] = ... or let s: String = ...
                    if let Some(ref te) = ty {
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
                        return self.compile_set_new_stmt(&name);
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
                let is_fresh_construction = rhs_yields_fresh_ref(value);
                let rhs_is_fstring = matches!(&value.kind, ExprKind::InterpolatedStringLit(_));
                // Thread the binding's Vec element type through to
                // `Vec.with_capacity(n)` in the RHS — the zero-arg
                // constructor can't recover `T` from arguments, but
                // `vec_elem_types[var_name]` is already populated above
                // from the annotation (or pattern_binding_inner_types
                // for the no-annotation path). Cleared after compile.
                let saved_pending_let_elem = self.pending_let_elem_type.take();
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    if let Some(&elem_ty) = self.vec_elem_types.get(var_name.as_str()) {
                        self.pending_let_elem_type = Some(elem_ty);
                    }
                }
                let val = self.compile_expr(value)?;
                self.pending_let_elem_type = saved_pending_let_elem;
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
                        // (b) Untyped let with a free-fn call RHS whose
                        //     return type is `Option[shared T]`. The
                        //     declare_function pass recorded the inner
                        //     shared name in `fn_return_option_inner_shared`.
                        if shared_option_info.is_none() {
                            if let ExprKind::Call { callee, .. } = &value.kind {
                                if let ExprKind::Identifier(fn_name) = &callee.kind {
                                    if let Some(inner_name) = self
                                        .fn_return_option_inner_shared
                                        .get(fn_name.as_str())
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
                                let obj_type_name: Option<String> = self
                                    .shared_type_for_call_like(object)
                                    .map(|(n, _)| n)
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
                        let ast_hint = if let ExprKind::Call { callee, .. } = &value.kind {
                            if let ExprKind::Path { segments, .. } = &callee.kind {
                                if segments.len() == 2 {
                                    let target = &segments[0];
                                    if let Some(target_st) = self.struct_types.get(target) {
                                        if *target_st == st {
                                            Some(target.clone())
                                        } else {
                                            None
                                        }
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        } else {
                            None
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
                // For shared types: rc_inc when copying from another variable (not fresh construction).
                if let Some((ref var_name, ref info)) = shared_info {
                    if !is_fresh_construction {
                        // Copying a shared pointer — increment refcount.
                        let ptr = val.into_pointer_value();
                        self.emit_refcount_inc(var_name, info.heap_type, ptr);
                    }
                    // Track for scope-exit cleanup.
                    let ptr = val.into_pointer_value();
                    self.track_rc_var(var_name, ptr, info.heap_type);
                }
                // RC-fallback boxing: heap-box non-shared bindings flagged by the ownership checker.
                // Skipped for Vec/String bindings (their inner buffers need separate cleanup).
                let val = if let PatternKind::Binding(var_name) = &pattern.kind {
                    let is_vec = self.vec_elem_types.contains_key(var_name.as_str());
                    if shared_info.is_none() && !is_vec && self.is_rc_fallback_binding(var_name) {
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
                // Track Vec variables for scope cleanup.
                if let PatternKind::Binding(var_name) = &pattern.kind {
                    if let Some(&elem_ty) = self.vec_elem_types.get(var_name.as_str()) {
                        if let Some(slot) = self.variables.get(var_name.as_str()) {
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
                            if !matches!(slot.ty, BasicTypeEnum::ArrayType(_)) {
                                self.track_vec_var(slot.ptr, Some(elem_ty));
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
                        self.suppress_source_vec_cleanup_for_arg(value);
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
                            if has_user_drop {
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
                let val = self.compile_expr(expr)?;
                // Phase-8 line 39 follow-up — `c.request(url).header(...);`
                // discards a live RequestBuilder temporary; free its
                // abandoned HTTP_BUILDERS handle (no-op for non-builder /
                // already-sent chains).
                self.free_discarded_request_builder_temp(expr, val);
                Ok(())
            }
            StmtKind::Assign { target, value } => {
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
                let rhs_is_fresh = rhs_yields_fresh_ref(value);
                let rhs_is_fstring = matches!(&value.kind, ExprKind::InterpolatedStringLit(_));
                let val = self.compile_expr(value)?;
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
                    // For shared types: rc_dec old value, rc_inc new value
                    // (only when the RHS is not itself a fresh-ref source).
                    if let Some(type_name) = self.var_type_names.get(name).cloned() {
                        if let Some(info) = self.shared_types.get(&type_name).cloned() {
                            if let Some(slot) = self.variables.get(name).copied() {
                                // rc_dec old pointer
                                let old_ptr = self
                                    .builder
                                    .build_load(
                                        self.context.ptr_type(AddressSpace::default()),
                                        slot.ptr,
                                        "old_rc",
                                    )
                                    .unwrap()
                                    .into_pointer_value();
                                self.emit_refcount_dec(name, info.heap_type, old_ptr);
                                // rc_inc new pointer — only when the RHS
                                // is an alias of an existing tracked ref.
                                if !rhs_is_fresh {
                                    let new_ptr = val.into_pointer_value();
                                    self.emit_refcount_inc(name, info.heap_type, new_ptr);
                                }
                                self.builder.build_store(slot.ptr, val).unwrap();
                                return Ok(());
                            }
                        }
                    }
                    // `Option[shared T]` Assign — symmetric to the
                    // plain shared-T arm above, but operating on the
                    // Option struct's tag + w0 inner pointer:
                    //   1. Load the old slot, branch on tag; if Some,
                    //      dec the old inner pointer.
                    //   2. Store the new Option value.
                    //   3. If the RHS is not a fresh-ref source
                    //      (i.e., not a `Some(...)` literal or other
                    //      Call/MethodCall — those already carry a
                    //      +1 transfer; see the let-stmt comment for
                    //      the +1 handshake), branch on the new
                    //      tag; if Some, inc the new inner pointer.
                    // Without this, `mut next_a: Option[Node]` styled
                    // reassignments (recursive kata: `next_a = n.next;`)
                    // strand the old ref and over-decrement at scope
                    // exit, hanging the program on chain access.
                    if let Some(heap_type) = self.var_option_shared_heap.get(name.as_str()).copied()
                    {
                        if let Some(slot) = self.variables.get(name.as_str()).copied() {
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
                            // ── Step 1: dec old inner if old is Some. ──
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
                            let old_do_bb =
                                self.context.append_basic_block(fn_val, "opt.assign.old.do");
                            let old_skip_bb = self
                                .context
                                .append_basic_block(fn_val, "opt.assign.old.skip");
                            self.builder
                                .build_conditional_branch(old_is_some, old_do_bb, old_skip_bb)
                                .unwrap();
                            self.builder.position_at_end(old_do_bb);
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
                            let old_is_null = self
                                .builder
                                .build_is_null(old_inner, "opt.assign.old.is_null")
                                .unwrap();
                            let old_real_do_bb = self
                                .context
                                .append_basic_block(fn_val, "opt.assign.old.real_do");
                            self.builder
                                .build_conditional_branch(old_is_null, old_skip_bb, old_real_do_bb)
                                .unwrap();
                            self.builder.position_at_end(old_real_do_bb);
                            self.emit_refcount_dec(name, heap_type, old_inner);
                            self.builder
                                .build_unconditional_branch(old_skip_bb)
                                .unwrap();
                            self.builder.position_at_end(old_skip_bb);
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
                    // Vec[Vec[T]] / Vec[String] elements get their inner
                    // buffers freed too — `emit_free_vec_buffer_if_owned`
                    // takes the registered elem_ty and does the
                    // recursive-drop walk inline. Without this, kata-17's
                    // K=100k Letter-Combinations workload retains 38.5
                    // MiB peak RSS instead of plateauing at the C/Rust
                    // working-set baseline of 1.3 MiB.
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
                        self.builder.build_store(slot.ptr, val).unwrap();
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
                        self.suppress_source_vec_cleanup_for_arg(value);
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
                    // `*r = val` — store through the mut-ref pointer.
                    // get_data_ptr loads the raw pointer from the alloca (one
                    // load, not two), giving us the address to store into.
                    if let ExprKind::Identifier(name) = &operand.kind {
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
        // Slice c-repl.B.5.3b: no slot-suppression for Map. Unlike
        // Vec/String which use a cap=0 sentinel in the slot's struct
        // triple, Map's cleanup is queue-driven (`FreeMapHandle` is
        // pushed to `scope_cleanup_actions` from a known set of
        // sites). Suppression happens at the registration site
        // instead — `compile_map_new_stmt` skips `track_map_var` when
        // `snapshot_capture.contains_key(var_name)`. The slot keeps
        // the live handle so same-cell `m.insert(...)` / `m.get(...)`
        // still find the Map; no nulling required.
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
            super::SnapshotPrimKind::Map { .. } => {
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
            | super::SnapshotPrimKind::Map { .. } => Ok(loaded),
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
            | super::SnapshotPrimKind::Map { .. } => Ok(loaded),
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
            super::SnapshotPrimKind::Map { .. } => {
                // Slice c-repl.B.5.3b: zero-initialize the handle
                // pointer. `karac_map_free` early-returns on a null
                // map, so an uncaptured global accidentally treated
                // as a Map slot is a safe no-op.
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
/// Conservative on mixed-shape branches: returns `false` when ANY branch
/// tail aliases an existing ref (`Identifier` / `FieldAccess` / `Index`
/// / etc.), so the receive site still incs. The fresh-tail branches in
/// that mix will double-inc (leaking +1 on those paths) — same behavior
/// as before this helper — but the aliasing branch is preserved
/// correctly. Per-branch inc emission would require lowering the
/// receive-inc into each tail block; deferred to a future slice.
pub(super) fn rhs_yields_fresh_ref(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::StructLiteral { .. } | ExprKind::Call { .. } | ExprKind::MethodCall { .. } => {
            true
        }
        ExprKind::Block(block)
        | ExprKind::Unsafe(block)
        | ExprKind::LabeledBlock { body: block, .. } => block
            .final_expr
            .as_deref()
            .is_some_and(rhs_yields_fresh_ref),
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
                .is_some_and(rhs_yields_fresh_ref)
                && else_branch.as_deref().is_some_and(rhs_yields_fresh_ref)
        }
        ExprKind::Match { arms, .. } => {
            !arms.is_empty() && arms.iter().all(|arm| rhs_yields_fresh_ref(&arm.body))
        }
        _ => false,
    }
}
