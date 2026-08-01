//! Block / par-block / statement evaluation and cleanup.
//!
//! Houses `eval_block_inner` (sequential block body + drop/defer
//! cleanup stack), `eval_par_block` (par-block lowering — task
//! cluster fork/join), `eval_stmt_cf` (one-statement dispatch),
//! `dispatch_lowered_op` (rewriting typechecker-lowered operator
//! method calls back into binop/unary), and the cleanup helpers
//! `run_cleanup`, `fire_due_drops`, `observed_cancellation`, and
//! `signal_cancellation_if_error`.
//!
//! Lives in a sibling `impl<'a> super::Interpreter<'a>` block.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::ast::*;
use crate::token::Span;

use super::exec::{
    compute_block_last_use, push_drops_for_stmt, CleanupAction, ControlFlow, ErrDeferEntry,
    EvalResult, ExitPath,
};
use super::value::{EnumData, Value};
use super::Interpreter;

impl<'a> super::Interpreter<'a> {
    /// REPL cross-cell snapshot capture (B-2026-07-29-20). Record every
    /// watched binding held by the scope that is about to pop.
    ///
    /// This used to fire from the `StmtKind::Let` arm, which froze each
    /// binding at its INITIALIZER: `let mut n: i64 = 0; n = 5;` crossed to
    /// the next cell as `0`, and `let mut m = Map.new(); m.insert(…)`
    /// crossed empty. `Vec` was the one row that looked right, and only by
    /// accident — `Value::Array` is an `Arc<RwLock<…>>`, so the clone taken
    /// at the `let` aliased the same storage and saw later pushes.
    ///
    /// Capturing at scope exit is uniform across all three representations
    /// and needs no per-type reasoning. It runs at every block's exit, not
    /// just `main`'s: `Env::scopes` is a plain stack, so an inner block
    /// shadowing a watched name would otherwise leave its value behind.
    /// Inner scopes pop FIRST, so `main`'s body block — the one holding the
    /// cell's top-level bindings — writes last and wins. Only names in the
    /// popping scope itself are read, so a callee's frame can't overwrite a
    /// caller's binding of the same name.
    ///
    /// No-op outside the REPL: `let_snapshot_watch` is empty everywhere
    /// else, so this is one `is_empty()` per scope exit.
    pub(crate) fn capture_watched_bindings(&mut self) {
        if self.let_snapshot_watch.is_empty() {
            return;
        }
        let Some(scope) = self.env.scopes.last() else {
            return;
        };
        let found: Vec<(String, Value)> = scope
            .iter()
            .filter(|(n, _)| self.let_snapshot_watch.contains(n.as_str()))
            .map(|(n, v)| (n.clone(), v.clone()))
            .collect();
        for (name, val) in found {
            self.captured_let_values.insert(name, val);
        }
    }

    #[allow(clippy::result_large_err)]
    pub(crate) fn eval_block_inner(&mut self, block: &Block) -> EvalResult {
        self.env.push_scope();
        // Unified drop+defer cleanup stack — entries pushed in program-order
        // as control flow reaches each binding/defer statement, drained LIFO
        // at scope exit. Per design.md § Drop ordering within a branch:
        // destructors and `defer` blocks interleave in this single stack,
        // ordered by program-order of introduction. `errdefer` lives on a
        // separate phase-1 stack that drains first on error paths.
        let mut cleanup: Vec<CleanupAction> = Vec::new();
        let mut errdefers: Vec<ErrDeferEntry> = Vec::new();
        // B-2026-07-30-11 (match-arm leg): adopt the taken match arm's
        // moved-payload bindings — stashed by `eval_match` immediately before
        // this block (the arm body) evaluates — as ordinary Drop slots. From
        // here on they are indistinguishable from `let`-introduced bindings:
        // the NLL last-use fire, the LIFO scope drain, and every
        // move-suppression hook (let-rebind, ctor args, `return`) apply
        // unchanged. Empty for every block that is not a match arm body.
        for name in std::mem::take(&mut self.pending_arm_drop_bindings) {
            cleanup.push(CleanupAction::Drop { name });
        }
        // Sub-step 3 (NLL placement): pre-compute each owned binding's
        // last-use statement index. After every successful statement,
        // any `Drop` slot whose binding's last use was that statement
        // fires immediately (and is removed from `cleanup`), instead
        // of waiting for scope exit. Bindings whose last-use is the
        // sentinel `stmts.len()` (referenced in `final_expr` or in a
        // defer/errdefer body) stay in `cleanup` and drain via the
        // unified LIFO at scope exit, preserving the program-order
        // interleave with Defers for that case.
        let last_use = compute_block_last_use(block);

        for (stmt_idx, stmt) in block.stmts.iter().enumerate() {
            // `defer` / `errdefer` register their bodies at the moment
            // control flow reaches the statement — *not* at block start.
            // A defer below an early `return` is therefore never registered,
            // matching design.md (and Go/Zig semantics).
            match &stmt.kind {
                StmtKind::Defer { body } => {
                    cleanup.push(CleanupAction::Defer(body.clone()));
                    continue;
                }
                StmtKind::ErrDefer { binding, body } => {
                    errdefers.push(ErrDeferEntry {
                        binding: binding.clone(),
                        body: body.clone(),
                    });
                    continue;
                }
                _ => {}
            }
            // par {}-cancellation effect-boundary check. When this
            // interpreter is acting as a sibling branch and another
            // sibling has signalled fail-fast, raise Cancelled so the
            // active scope's errdefer phase fires with e = Cancelled.
            if self.observed_cancellation() {
                let cf = ControlFlow::Cancelled;
                let path = ExitPath::classify(&cf);
                self.run_cleanup(&cleanup, &errdefers, &path);
                self.capture_watched_bindings();
                self.env.pop_scope();
                return Err(cf);
            }
            // `karac test` per-test deadline check. Polled here so a
            // runaway loop or deadlocked test surfaces at the next
            // statement boundary; cleanup still drains via the unified
            // stack but errdefer is bypassed (TimedOut classifies as
            // Normal, since the deadline is a runner-side guardrail,
            // not a user-visible error path). The `timed_out` flag
            // signals to the runner that the outcome is a timeout
            // rather than a normal completion.
            if self.observed_test_deadline_exceeded() {
                self.timed_out = true;
                let cf = ControlFlow::TimedOut;
                let path = ExitPath::classify(&cf);
                self.run_cleanup(&cleanup, &errdefers, &path);
                self.capture_watched_bindings();
                self.env.pop_scope();
                return Err(cf);
            }
            // Sub-slice (3) of move-suppression — pre-statement
            // suppression for `return expr;` where expr is an
            // Identifier whose binding has a user `impl Drop`. The
            // source's value is moved out as the return value; its
            // Drop slot is removed from cleanup BEFORE the statement
            // evaluates so when run_cleanup fires (after the
            // ControlFlow::Return signal propagates back to this
            // block), the source's user-body doesn't run.
            self.suppress_return_stmt_user_drop(stmt, &mut cleanup);
            // Container twin of the line above: `return a;` moves a's
            // container value to the caller — record it so the payload/
            // element-body walks skip when the return's cleanup drains
            // (the caller's binding runs them on the same logical value).
            if let StmtKind::Expr(e) = &stmt.kind {
                if let ExprKind::Return(Some(inner)) = &e.kind {
                    self.record_container_bodies_move_sources(inner);
                }
            }
            let stmt_result = self.eval_stmt_cf(stmt);
            let cf_opt = match stmt_result {
                Ok(_) => self.pending_cf.take(),
                Err(cf) => Some(cf),
            };
            if let Some(cf) = cf_opt {
                let path = ExitPath::classify(&cf);
                // Notify sibling par-branches as soon as the error
                // path is detected, not after the branch finishes —
                // that way a still-running sibling can observe the
                // flag at its next between-statement check.
                self.signal_cancellation_if_error(&cf);
                self.run_cleanup(&cleanup, &errdefers, &path);
                self.capture_watched_bindings();
                self.env.pop_scope();
                return Err(cf);
            }
            // Move-suppression for user-Drop bindings: when the let
            // statement's RHS is an Identifier whose value is a
            // user-Drop struct, the source binding's value has moved
            // into the destination. Suppress the source's Drop action
            // so its user body doesn't fire at scope exit (the
            // destination's drop fires on the same logical value
            // instead, exactly once). Sibling of codegen's
            // `suppress_user_drop_for_var` in `src/codegen/runtime.rs`.
            // Pre-existing non-user-Drop bindings still get their
            // drop_trace records — gated on `drop_method_keys` so the
            // NLL placement / scope-exit ordering tests for plain
            // bindings stay unchanged.
            self.suppress_let_rebind_user_drop(stmt, &mut cleanup);
            self.suppress_discarded_tuple_moved_elem_user_drops(stmt, &mut cleanup);
            // B-2026-07-30-11 (displaced-value leg): `a = b;` moves b's value
            // into `a` exactly as the let-rebind above does — the source's
            // Drop slot must be retracted or its body fires a second time on
            // the value now owned by `a`. Same identifier-RHS rule, same
            // user-Drop gate; the Assign arm itself already ran the DISPLACED
            // old `a` value's body before the store.
            self.suppress_assign_move_user_drop(stmt, &mut cleanup);
            // B-2026-07-29-39 — `let x = h.a;` moves a Drop-bearing FIELD out
            // of `h`, so `h` must stop running that field's body (`x` runs it
            // now). Mirrors codegen's
            // `disarm_user_drop_fields_for_moved_field`.
            self.suppress_moved_out_drop_field(stmt);
            // Move-suppression for `forget(x);` — the FFI ownership-handoff
            // primitive removes the source binding's Drop slot so the
            // destructor never fires (Slice 4).
            self.suppress_forget_stmt_user_drop(stmt, &mut cleanup);
            // After a successful let-binding, push a Drop slot for each
            // name the pattern introduced. EXCEPT (B-2026-08-01-12): a
            // struct destructure of an OWNED PARAM (`let Holder { r } = h;`
            // where `h` is the current fn's by-value param) binds views of
            // the callee's entry copy — under the caller-retains convention
            // the conceptual value's Drop observability is the CALLER's
            // (its NLL / fresh-arg fire reads the original), and codegen
            // already fires caller-side only. Registering slots here
            // double-fired the bound fields' bodies under `karac run`.
            if !self.let_destructures_owned_param(stmt) {
                push_drops_for_stmt(stmt, &mut cleanup);
            }
            // NLL placement: fire any Drop slot whose binding's last
            // use was this statement, then remove it from `cleanup`
            // so it does not fire again at scope exit. A binding that
            // is never read (last_use == its own let stmt_idx) drops
            // here too — that's the "let _ = expensive(); …" case
            // where NLL says the value dies at its declaration.
            self.fire_due_drops(&mut cleanup, &last_use, stmt_idx);
        }
        let result = if let Some(ref expr) = block.final_expr {
            if self.observed_cancellation() {
                let cf = ControlFlow::Cancelled;
                let path = ExitPath::classify(&cf);
                self.run_cleanup(&cleanup, &errdefers, &path);
                self.capture_watched_bindings();
                self.env.pop_scope();
                return Err(cf);
            }
            if self.observed_test_deadline_exceeded() {
                self.timed_out = true;
                let cf = ControlFlow::TimedOut;
                let path = ExitPath::classify(&cf);
                self.run_cleanup(&cleanup, &errdefers, &path);
                self.capture_watched_bindings();
                self.env.pop_scope();
                return Err(cf);
            }
            // Sub-slice (3) of move-suppression — when the block's
            // trailing expression is an Identifier whose binding has
            // a user `impl Drop`, the source's value is moved out as
            // the block's result (return value for a function body).
            // Suppress its Drop in the cleanup vec so the user-body
            // doesn't fire when this block's `run_cleanup` runs after
            // returning — the receiving scope will fire it when its
            // own binding for the returned value goes out of scope.
            // Mirrors the codegen `suppress_cleanup_for_tail_return`
            // wiring.
            self.suppress_tail_expr_user_drop(expr, &mut cleanup);
            // Container twin: a bare-identifier tail moves the container out
            // as the block's result.
            self.record_container_bodies_move_sources(expr);
            let v = self.eval_expr_inner(expr);
            if let Some(cf) = self.pending_cf.take() {
                let path = ExitPath::classify(&cf);
                self.signal_cancellation_if_error(&cf);
                self.run_cleanup(&cleanup, &errdefers, &path);
                self.capture_watched_bindings();
                self.env.pop_scope();
                return Err(cf);
            }
            v
        } else {
            Value::Unit
        };
        // Normal exit — drop+defer phase only.
        self.run_cleanup(&cleanup, &errdefers, &ExitPath::Normal);
        self.capture_watched_bindings();
        self.env.pop_scope();
        Ok(result)
    }

    /// Execute a `par {}` block with parallel execution.
    /// Each top-level statement in the block becomes a concurrent branch.
    /// Fail-fast: first error cancels all siblings.
    #[allow(clippy::result_large_err)]
    pub(crate) fn eval_par_block(&mut self, block: &Block) -> EvalResult {
        let stmts = &block.stmts;

        // Single or zero statements — no parallelism needed
        if stmts.len() <= 1 {
            return self.eval_block_inner(block);
        }

        // Snapshot current environment for all branches
        let env_snapshot = self.env.snapshot();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let program = self.program;
        let typecheck_result = self.typecheck_result;
        let sequential_mode = self.sequential_mode;
        let source_filename = &self.source_filename;
        let source_text = &self.source_text;
        let dbg_output_mode = self.dbg_output_mode;
        let task_id_counter = Arc::clone(&self.task_id_counter);
        let parent_captures_dbg = self.captured_dbg.is_some();
        // Pre-allocate task ids in source order so a given branch always
        // reports the same task_id regardless of OS scheduling. The
        // counter is a monotonic Arc shared across nested par blocks; we
        // claim a contiguous range here, then each branch reads its
        // pre-assigned slot below.
        let branch_task_ids: Vec<u64> = (0..stmts.len())
            .map(|_| task_id_counter.fetch_add(1, Ordering::Relaxed) + 1)
            .collect();

        // Collect results from each branch
        // Each branch result: (index, defined_vars, output_lines, dbg_lines, control_flow_or_value)
        type BranchResult = (
            usize,
            HashMap<String, Value>,
            Vec<String>,
            Vec<String>,
            Result<Value, ControlFlow>,
        );
        let results: Mutex<Vec<BranchResult>> = Mutex::new(Vec::new());

        std::thread::scope(|s| {
            for (i, stmt) in stmts.iter().enumerate() {
                let env_snap = &env_snapshot;
                let cancel = Arc::clone(&cancel_flag);
                let prog = &program;
                let tc = &typecheck_result;
                let results_ref = &results;
                let stmt_clone = stmt.clone();
                let task_id_counter = Arc::clone(&task_id_counter);
                let task_id = branch_task_ids[i];
                s.spawn(move || {
                    // Pre-start cancellation observation: a sibling already
                    // failed before this branch was scheduled. The branch
                    // never enters its body, so no errdefers are registered
                    // and no cleanup runs — push nothing.
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }

                    // Create a branch interpreter with the shared env snapshot
                    let mut branch_interp = Interpreter::new(prog, tc);
                    branch_interp.captured_output = Some(Vec::new());
                    branch_interp.sequential_mode = sequential_mode;
                    branch_interp.source_filename = source_filename.clone();
                    branch_interp.cancel_flag = Some(Arc::clone(&cancel));
                    branch_interp.source_text = source_text.clone();
                    branch_interp.dbg_output_mode = dbg_output_mode;
                    branch_interp.task_id_counter = Arc::clone(&task_id_counter);
                    // Task id is pre-assigned in source order above so
                    // dbg() output reports a stable id for a given
                    // branch regardless of OS scheduling. Counter
                    // starts at 1 (id 0 is the "no par" sentinel,
                    // never reported as an actual task tag).
                    branch_interp.current_task_id = Some(task_id);
                    if parent_captures_dbg {
                        branch_interp.captured_dbg = Some(Vec::new());
                    }

                    // Restore environment snapshot
                    for (k, v) in env_snap {
                        branch_interp.env.define(k.clone(), v.clone());
                    }
                    // Register top-level items so function calls work
                    branch_interp.register_items();

                    // Execute the statement
                    let result = branch_interp.eval_stmt_cf(&stmt_clone);
                    // Also check pending_cf
                    let cf_result = if let Some(cf) = branch_interp.pending_cf.take() {
                        Err(cf)
                    } else {
                        result.map(|_| Value::Unit)
                    };

                    // On error, set cancel flag for fail-fast
                    if cf_result.is_err() {
                        cancel.store(true, Ordering::Relaxed);
                    }

                    // Collect defined variables from this branch (top scope only)
                    let defined_vars = if let Some(scope) = branch_interp.env.scopes.last() {
                        scope.clone()
                    } else {
                        HashMap::new()
                    };

                    let output = branch_interp.captured_output.unwrap_or_default();
                    let dbg_lines = branch_interp.captured_dbg.unwrap_or_default();

                    results_ref.lock().unwrap().push((
                        i,
                        defined_vars,
                        output,
                        dbg_lines,
                        cf_result,
                    ));
                });
            }
        });

        // Sort results by source order (deterministic)
        let mut branch_results = results.into_inner().unwrap();
        branch_results.sort_by_key(|(i, _, _, _, _)| *i);

        // Merge results back into the parent interpreter
        // 1. Merge output in source order
        for (_, _, output, _, _) in &branch_results {
            for line in output {
                if let Some(ref mut cap) = self.captured_output {
                    cap.push(line.clone());
                } else {
                    print!("{}", line);
                }
            }
        }

        // 1b. Merge dbg lines in source order (test-only; only present
        // when the parent has an active capture buffer).
        if let Some(ref mut cap) = self.captured_dbg {
            for (_, _, _, dbg_lines, _) in &branch_results {
                for line in dbg_lines {
                    cap.push(line.clone());
                }
            }
        }

        // 2. Merge defined variables into the CURRENT (enclosing) scope so
        //    they outlive the `par {}` block — the join barrier hoists each
        //    branch's `let` into the enclosing scope, matching the resolver /
        //    typechecker and the shape `par { let a = f(); let b = g(); }
        //    (a, b)` needs (B-2026-07-11-3). No private scope is pushed.
        for (_, vars, _, _, _) in &branch_results {
            for (name, val) in vars {
                // Skip prelude/function definitions
                if matches!(val, Value::Function { .. } | Value::EnumVariant { .. }) {
                    continue;
                }
                self.env.define(name.clone(), val.clone());
            }
        }

        // 3. Check for errors (fail-fast: first error in source order).
        // `ControlFlow::Cancelled` is silenced — a cancelled sibling's
        // cleanup already ran with `e = Cancelled`, but the originating
        // branch's real `Err` is what propagates as the scope's value.
        for (_, _, _, _, result) in branch_results {
            if let Err(cf) = result {
                if matches!(cf, ControlFlow::Cancelled) {
                    continue;
                }
                return Err(cf);
            }
        }

        // 4. Final expression (usually absent — the join hoists bindings into
        //    the enclosing scope for use after the block; the tail form
        //    `par { …; (a, b) }` still evaluates its value here).
        let result = if let Some(ref expr) = block.final_expr {
            let v = self.eval_expr_inner(expr);
            if let Some(cf) = self.pending_cf.take() {
                return Err(cf);
            }
            v
        } else {
            Value::Unit
        };
        Ok(result)
    }

    /// True iff this interpreter is acting as a `par {}` sibling branch
    /// and a peer has signalled fail-fast cancellation.
    fn observed_cancellation(&self) -> bool {
        self.cancel_flag
            .as_ref()
            .map(|f| f.load(Ordering::Relaxed))
            .unwrap_or(false)
    }

    /// True iff `karac test` set a per-test deadline and the current
    /// wall-clock time has reached or passed it. Polled at the same
    /// between-statement boundaries as `observed_cancellation()` so a
    /// timeout from a runaway loop / deadlock surfaces within one
    /// statement of when it crosses the deadline. `None` deadline
    /// (no test runner, or runner explicitly disabled timeouts) → no
    /// check, zero overhead per statement.
    fn observed_test_deadline_exceeded(&self) -> bool {
        match self.test_deadline {
            Some(deadline) => std::time::Instant::now() >= deadline,
            None => false,
        }
    }

    /// Set the shared `par {}` cancel flag (if any) when the active
    /// scope is unwinding on an error path. Cancellation is itself an
    /// error path but the store is idempotent.
    fn signal_cancellation_if_error(&self, cf: &ControlFlow) {
        let is_error_path = matches!(
            cf,
            ControlFlow::Return(Value::EnumVariant { variant, .. })
                if variant == "Err" || variant == "None"
        ) || matches!(
            cf,
            ControlFlow::RuntimeError | ControlFlow::ExitUnwind { .. } | ControlFlow::Cancelled
        );
        if is_error_path {
            if let Some(ref flag) = self.cancel_flag {
                flag.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Drain the unified drop+defer cleanup stack at scope exit per
    /// design.md § Drop ordering within a branch. Two phases:
    ///
    /// 1. `errdefer` phase (error paths only). Param-less `errdefer { ... }`
    ///    runs on every error path. `errdefer(e) { ... }` binds `e` to the
    ///    propagating `Err` payload (or `Cancelled` in cancelled siblings —
    ///    sub-step 4 wires that branch). `errdefer(e)` is skipped on panic
    ///    per the language rules.
    /// 2. drop+defer phase (always). Drains the unified stack LIFO so
    ///    `let x = ...; defer foo();` cleans up as `foo()` then `drop(x)`.
    ///    `Drop` actions record on `drop_trace`; once user-`impl Drop`
    ///    dispatch lands, observable side effects attach here without
    ///    changing the program-order LIFO position.
    fn run_cleanup(
        &mut self,
        cleanup: &[CleanupAction],
        errdefers: &[ErrDeferEntry],
        path: &ExitPath,
    ) {
        // Phase 1: errdefer. Reverse declaration order; param-less runs on
        // every error path, errdefer(e) binds the Err payload (skipped on
        // panic — only param-less fires there).
        if path.is_error() {
            for entry in errdefers.iter().rev() {
                match &entry.binding {
                    Some(name) => match path {
                        ExitPath::Err(payload) | ExitPath::Cancelled(payload) => {
                            self.env.push_scope();
                            self.env.define(name.clone(), payload.clone());
                            let _ = self.eval_block_inner(&entry.body);
                            self.env.pop_scope();
                        }
                        ExitPath::Panic | ExitPath::NoneProp | ExitPath::Normal => {
                            // errdefer(e) is skipped on panic and on bare
                            // None propagation (no payload to bind).
                        }
                    },
                    None => {
                        let _ = self.eval_block_inner(&entry.body);
                    }
                }
            }
        }
        // Phase 2: drop+defer interleaved LIFO.
        for action in cleanup.iter().rev() {
            match action {
                CleanupAction::Defer(body) => {
                    let _ = self.eval_block_inner(body);
                }
                CleanupAction::Drop { name } => {
                    // Phase 7 user-`impl Drop` dispatch Prereq.4 — fire
                    // the user-defined drop body BEFORE recording the
                    // trace so observable side effects (e.g. println
                    // from `fn drop()`) are visible to the test. The
                    // helper is a no-op when the binding's type has
                    // no user `impl Drop`, preserving the
                    // no-impl-Drop behaviour at this drain.
                    self.invoke_user_drop_if_applicable(name);
                    self.drop_trace.push(name.clone());
                }
            }
        }
    }

    /// Fire any `Drop` slot whose binding's last use was the just-
    /// finished statement, and remove it from `cleanup` so it does
    /// not fire again at scope exit. NLL placement per design.md §
    /// Drop ordering within a branch (sub-step 3). `Defer` slots
    /// always stay in `cleanup` and drain at scope exit. Walks
    /// `cleanup` BACK-TO-FRONT so multiple drops due at the SAME
    /// statement fire in LIFO (reverse-introduction) order — the
    /// design.md § 867 single-stack rule ("drain in the same LIFO
    /// stack ... ordered by program-order of introduction") applies
    /// to NLL firings too, and codegen's `fire_due_user_drops`
    /// mirrors this exact order (B-2026-07-21-1; the previous
    /// front-to-back walk fired same-statement drops FIFO). In-place
    /// removal keeps the relative order of remaining entries
    /// unchanged. Drop firings are recorded on `drop_trace` directly
    /// here (rather than via `run_cleanup`) so test traces include
    /// NLL and scope-exit firings in their actual program order.
    fn fire_due_drops(
        &mut self,
        cleanup: &mut Vec<CleanupAction>,
        last_use: &HashMap<String, usize>,
        stmt_idx: usize,
    ) {
        let mut i = cleanup.len();
        while i > 0 {
            i -= 1;
            let should_fire = match &cleanup[i] {
                CleanupAction::Drop { name } => last_use.get(name).copied() == Some(stmt_idx),
                CleanupAction::Defer(_) => false,
            };
            if should_fire {
                let action = cleanup.remove(i);
                if let CleanupAction::Drop { name } = action {
                    // Phase 7 user-`impl Drop` dispatch Prereq.4 — fire
                    // the user body at NLL endpoint before pushing the
                    // trace record, mirroring the scope-exit drain
                    // arm in `run_cleanup`.
                    self.invoke_user_drop_if_applicable(&name);
                    self.drop_trace.push(name);
                }
            }
        }
    }

    /// Phase 7 user-`impl Drop` dispatch Prereq.4 — invoke the
    /// user-defined `<Type>.drop` method body on a binding before its
    /// `CleanupAction::Drop` slot drains. No-op when the binding doesn't
    /// resolve to a `Value::Struct`, when its type isn't in
    /// `program.drop_method_keys`, or when the method symbol isn't
    /// present in the environment (the typechecker's `drop_method_keys`
    /// is the authoritative gate — only validated impls reach it, so
    /// the env lookup should always succeed when the gate fires).
    /// Mirrors the codegen drain at `src/codegen/runtime.rs`'s
    /// `CleanupAction::UserDrop` arm: the user body runs, then field
    /// cleanup follows (the interpreter's value model already releases
    /// heap-owned fields when the binding's `Value::Struct` is dropped
    /// at scope-exit Rust-level GC).
    /// B-2026-07-30-11 — run each ELEMENT's user `impl Drop` body when a
    /// `Vec`/`VecDeque` binding dies. Returns `true` when `name` resolved to an
    /// array, so the caller stops (an array is not a `drop_target` shape).
    ///
    /// `Env::drop_target` reports only `Struct` / `SharedStruct` /
    /// `EnumVariant`, so an array binding resolved to `None` and the whole hook
    /// early-returned: `let v: Vec[Res] = [...]` ran nothing when `v` died and
    /// every element's resource was held for the program's lifetime.
    ///
    /// FORWARD order (`0..len`), matching codegen's `__karac_dropelems_<T>`
    /// walk and the memory drain's. Struct FIELDS drop in reverse declaration
    /// order; container ELEMENTS do not, and both backends agree on the split.
    ///
    /// The element list is cloned out before the walk so a body that touches
    /// the same container cannot deadlock against a held read guard.
    fn run_array_element_user_drops(&mut self, name: &str) -> bool {
        let elems: Vec<Value> = match self.env.get(name) {
            Some(Value::Array(cell)) => match cell.read() {
                Ok(g) => g.clone(),
                Err(_) => return true,
            },
            // B-2026-07-30-11 (tuple leg) — a tuple binding's ELEMENTS, same
            // treatment and the same forward order. Reached only for `let`
            // bindings, because `push_drops_for_stmt` registers a `Drop` action
            // only for those — which is exactly the position codegen's tuple
            // registration covers, so the two stay in step.
            Some(Value::Tuple(items)) => items,
            _ => return false,
        };
        // Whole-value move-out (`let v2 = v;`, `return v;`): the destination
        // owns the elements; walking here too fires each body twice (and
        // codegen's walk would read the moved-from slot). Checked after the
        // shape resolution so the caller still stops for an array/tuple.
        if self.moved_out_container_bodies_bindings.contains(name) {
            return true;
        }
        for e in elems {
            if let Value::Struct { name: tn, .. } = &e {
                if self.program.drop_method_keys.contains_key(tn) {
                    let tn = tn.clone();
                    self.run_user_drop_body_on_value(&tn, e);
                    continue;
                }
            }
            if self.value_runs_user_drop(&e) {
                self.drop_user_drop_fields_of_value(&e);
            }
        }
        true
    }

    fn invoke_user_drop_if_applicable(&mut self, name: &str) {
        // A binding whose whole value moved into a variant constructor runs
        // NOTHING — own body or walks — the enum's owner does (B-2026-07-30-11
        // Option/Result leg; codegen twin: `suppress_user_drop_for_var` at
        // the ctor arg loop).
        if self.moved_out_user_drop_bindings.contains(name) {
            return;
        }
        // B-2026-07-30-11 — a container binding never resolved through
        // `drop_target`, so its elements' bodies never ran. Checked first: an
        // array is not one of the shapes that function reports on at all.
        if self.run_array_element_user_drops(name) {
            return;
        }
        // Map-values leg: same treatment for a `Map` binding's stored values.
        if self.run_map_val_user_drops(name) {
            return;
        }
        // Resolve the binding's type (and, for a shared struct, its Arc
        // strong-count) WITHOUT cloning — cloning the value first would
        // bump a shared struct's refcount and break the last-reference
        // test below. See `Environment::drop_target`.
        let (type_name, shared_count) = match self.env.drop_target(name) {
            Some(t) => t,
            None => return,
        };
        let has_user_drop = self.program.drop_method_keys.contains_key(&type_name);
        // Shared struct: fire the user body at refcount→0, mirroring
        // codegen's `emit_rc_dec` free branch. `drop_target` reports the
        // live count; `== 1` means this binding holds the sole reference
        // and is the last drop. To let a *later* alias's drain reach 1,
        // release THIS binding's `Arc` from env after handling it — a
        // drained binding is at its NLL endpoint (or scope exit), so its
        // slot is dead and removal is safe. Without the release every
        // alias of `let r2 = r` lingers in env until scope pop, the count
        // never reaches 1, and the body would never fire. A return-value
        // clone (tail escape) keeps the count > 1 here, so the body fires
        // exactly once — when the final holder drops. Recursive /
        // field-held inner refs (held inside another shared struct's
        // field, not an env binding) still never reach a drain and need an
        // Arc-drop hook; codegen handles them — the interpreter gap is
        // tracked under the L940 drop-reconciliation item.
        if let Some(count) = shared_count {
            if has_user_drop {
                if count == 1 {
                    self.run_user_drop_body(&type_name, name);
                }
                self.env.remove_local(name);
            }
            return;
        }
        if !has_user_drop {
            // B-2026-07-29-39 — the binding's own type declares no `Drop`, but
            // a FIELD's type may. That is the whole bug: drop glue dispatched a
            // user body for a direct binding of a Drop type and never walked an
            // aggregate's fields, so `struct Holder { r: Res }` dropped nothing
            // when `h` died and every resource held in a field leaked for the
            // program's lifetime. This must sit OUTSIDE the early-out, not
            // after the body call below.
            self.drop_user_drop_fields_of_binding(name);
            return;
        }
        // A `#[compiler_builtin]` stdlib `impl Drop` (e.g.
        // `PooledConnection`) releases a side-table resource the
        // interpreter owns rather than running a Kāra body, so route it
        // to the native handler before the (placeholder) body drain.
        if self.try_eval_builtin_drop(&type_name, name) {
            return;
        }
        // Parent body first, then its fields die (design.md § Drop ordering),
        // exactly like codegen's `karac_drop_<T>` wrapper. Split rather than
        // routed through `run_user_drop_body_on_value` so the field walk goes
        // through the NAME-keyed entry point, which is what consults the
        // moved-out-field set (`let x = h.a;` hands that body to `x`).
        self.run_user_drop_body(&type_name, name);
        self.drop_user_drop_fields_of_binding(name);
    }

    /// B-2026-07-29-39 — run the user `impl Drop` of every Drop-bearing FIELD
    /// reachable from a dying binding. Mirrors the codegen pass
    /// `emit_user_drop_field_bodies_fn`, including its order (reverse
    /// declaration) and its exclusions, so `karac run` and `karac build`
    /// produce the same output — the observable signal for a user drop, and
    /// what the parity gate asserts.
    fn drop_user_drop_fields_of_binding(&mut self, name: &str) {
        if self.moved_out_drop_field_bindings.contains(name) {
            return;
        }
        let Some(value) = self.env.get(name) else {
            return;
        };
        // B-2026-07-30-11 (enum leg) — an enum binding's live-variant PAYLOAD
        // bodies. Handled here, at the NAME-keyed entry point, rather than
        // inside the value-level `drop_user_drop_fields_of_value`: that walker
        // has several other callers (discarded temps, container elements) and
        // widening it would fire on shapes codegen's let-site-only registration
        // does not cover. This entry point is reached only from
        // `invoke_user_drop_if_applicable`, whose `Drop` action
        // `push_drops_for_stmt` registers only for `let` bindings — the same
        // position `__karac_dropelems_enum_<E>` is registered at, so the two
        // backends cover exactly this and no more.
        if matches!(value, Value::EnumVariant { .. }) {
            self.run_enum_payload_user_drops(name, &value);
            return;
        }
        self.drop_user_drop_fields_of_value(&value);
    }

    /// B-2026-07-30-11 (enum leg) — run the user `impl Drop` body of each
    /// Drop-bearing payload of `value`'s live variant, in forward declaration
    /// order (matching codegen's `__karac_dropelems_enum_<E>` walk).
    ///
    /// No-op once `name` is in `moved_out_enum_payload_bindings` — a `match` /
    /// `if let` arm bound the payload out, so the source no longer owns it.
    /// Codegen's twin is the prefix-keyed `suppress_container_elem_bodies_for_var`
    /// retraction, and this shares its coarseness deliberately: the flag is set
    /// when ANY arm consumes a Drop-bearing payload, not only the arm actually
    /// taken, because codegen's retraction is a compile-time removal that
    /// cannot be path-sensitive. Firing here on a non-consuming sibling arm
    /// would print a body `karac build` does not.
    fn run_enum_payload_user_drops(&mut self, name: &str, value: &Value) {
        if self.moved_out_enum_payload_bindings.contains(name)
            // Whole-value move-out (`let b = a;`, `x = a;`, `return a;`) —
            // the destination's walk owns the payload bodies now.
            || self.moved_out_container_bodies_bindings.contains(name)
        {
            return;
        }
        let Value::EnumVariant {
            enum_name,
            variant,
            data,
        } = value
        else {
            return;
        };
        // B-2026-07-30-11 (Option/Result leg): `Option`/`Result` are built-in
        // — no source-level `EnumDef`, and their declared payload is the bare
        // generic param, so the declared-type walk below can never fire for
        // them. Their gate is INSTANTIATION-driven instead, off the te the
        // Let arm recorded through codegen's exact resolution chain.
        if enum_name == "Option" || enum_name == "Result" {
            let variant = variant.clone();
            let data = data.clone();
            self.run_optres_payload_user_drops(name, &variant, &data);
            return;
        }
        self.run_enum_payload_user_drops_value(value);
    }

    /// Value-level core of [`Self::run_enum_payload_user_drops`] — the
    /// declared-type-driven payload-body walk with no binding-name disarm
    /// checks, for values that never had a binding (discarded temps:
    /// `let _ = mk_enum();`, `mk_enum();`). Codegen twin:
    /// `__karac_dropelems_enum_<E>` registered on the discard frame
    /// (B-2026-08-01-2). `Option`/`Result` have no source `EnumDef`, so
    /// `variant_payload_decls` yields `None` and this is a no-op for them —
    /// their discard path stays with the value-driven recursion in
    /// `run_discarded_value_user_drops`, twin to codegen's
    /// instantiation-driven optres registrar.
    pub(super) fn run_enum_payload_user_drops_value(&mut self, value: &Value) {
        let Value::EnumVariant {
            enum_name,
            variant,
            data,
        } = value
        else {
            return;
        };
        let Some(decls) = self.variant_payload_decls(enum_name, variant) else {
            return;
        };
        // (declared head type, payload value) for each declared position.
        let payloads: Vec<(Option<String>, Value)> = match data {
            EnumData::Unit => return,
            EnumData::Tuple(items) => decls
                .iter()
                .enumerate()
                .filter_map(|(i, (_, te))| {
                    items
                        .get(i)
                        .map(|v| (Self::declared_field_type_head(te), v.clone()))
                })
                .collect(),
            EnumData::Struct(fields) => decls
                .iter()
                .filter_map(|(fname, te)| {
                    fields
                        .get(fname.as_deref()?)
                        .map(|v| (Self::declared_field_type_head(te), v.clone()))
                })
                .collect(),
        };
        for (declared_head, payload) in payloads {
            let Value::Struct { name: tn, .. } = &payload else {
                continue;
            };
            // Declared-type-driven, exactly like the struct-field walk: a
            // payload declared as a bare generic param is erased at the point
            // codegen emits its glue, so both backends skip it and the residual
            // is a leak — the safe direction.
            if declared_head.as_deref() != Some(tn.as_str()) {
                continue;
            }
            if self.program.drop_method_keys.contains_key(tn) {
                let tn = tn.clone();
                self.run_user_drop_body_only(&tn, payload.clone());
            }
            self.drop_user_drop_fields_of_value(&payload);
        }
    }

    /// `(field name, declared type)` for each payload position of
    /// `Enum.Variant`, in declaration order — `None` field name for a tuple
    /// variant. Scans the user program then the baked stdlib, the same two
    /// sources codegen's `enum_variant_field_type_exprs` consults.
    pub(crate) fn variant_payload_decls(
        &self,
        enum_name: &str,
        variant: &str,
    ) -> Option<Vec<(Option<String>, TypeExpr)>> {
        fn scan(
            items: &[Item],
            enum_name: &str,
            variant: &str,
        ) -> Option<Vec<(Option<String>, TypeExpr)>> {
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
        scan(&self.program.items, enum_name, variant).or_else(|| {
            crate::prelude::STDLIB_PROGRAMS
                .iter()
                .find_map(|(_, p)| scan(&p.items, enum_name, variant))
        })
    }

    /// B-2026-07-29-39 — record that a `let x = <src>.<field>;` moved a
    /// Drop-bearing field out of `<src>`, so `<src>`'s field walk skips it and
    /// only the destination binding runs the body.
    ///
    /// Coarse, exactly like codegen's
    /// `emit_user_drop_wrapper_without_field_bodies`: it disarms the source's
    /// WHOLE field walk, not just the moved field. For the common
    /// single-Drop-field aggregate that is exact; for a multi-field one it
    /// under-drops, which is the safe side of the trade — the two backends stay
    /// in step either way, which is what the parity gate pins.
    fn suppress_moved_out_drop_field(&mut self, stmt: &Stmt) {
        let StmtKind::Let { value, .. } = &stmt.kind else {
            return;
        };
        let ExprKind::FieldAccess { object, field, .. } = &value.kind else {
            return;
        };
        let ExprKind::Identifier(src) = &object.kind else {
            return;
        };
        let Some(Value::Struct { fields, .. }) = self.env.get(src) else {
            return;
        };
        let Some(field_value) = fields.get(field).cloned() else {
            return;
        };
        if self.value_runs_user_drop(&field_value) {
            self.moved_out_drop_field_bindings.insert(src.clone());
        }
    }

    /// The head type name of a field's DECLARED type (`r: Res` -> `Res`),
    /// or `None` for a non-path type. A bare generic param yields its own
    /// letter (`T`), which never matches a runtime struct name — which is how
    /// `drop_user_drop_fields_of_value` keeps erased fields out of the walk.
    fn declared_field_type_head(ty: &TypeExpr) -> Option<String> {
        match &ty.kind {
            TypeKind::Path(p) => p.segments.last().cloned(),
            _ => None,
        }
    }

    /// Does this value (or anything reachable through its struct fields) carry a
    /// user `impl Drop`? The interpreter's value-level twin of codegen's
    /// type-level `type_runs_user_drop`, so both backends disarm on the same
    /// condition. `Value::SharedStruct` is not walked — its drop is
    /// refcount-driven, never the holder's business.
    /// B-2026-08-01-12 — is this statement a struct destructure of an
    /// OWNED param of the currently-executing function
    /// (`let Holder { r } = h;` with `h` a by-value param)? The bound
    /// fields are views of the callee's entry copy; their Drop
    /// observability belongs to the caller (caller-retains), so
    /// `eval_block_inner` skips their Drop-slot registration. A
    /// destructure of a LOCAL (`let h2 = h; let Holder { r } = h2;`)
    /// stays registered — the move-indirected double is a recorded
    /// residual, matching codegen's depth-0 behavior.
    fn let_destructures_owned_param(&mut self, stmt: &Stmt) -> bool {
        let (pattern, value) = match &stmt.kind {
            StmtKind::Let { pattern, value, .. } => (pattern, value),
            // B-2026-08-01-13 widening: `let Full(r2) = w else { … }` on an
            // owned enum param is the same view-bind — codegen's let-else
            // route shares the pattern-binding param gate, so the interp
            // slot registration must stay silent too (the caller's
            // fresh-arg / NLL fire is the single owner).
            StmtKind::LetElse { pattern, value, .. } => (pattern, value),
            _ => return false,
        };
        // B-2026-08-01-15: a whole-move rebind of an owned param
        // (`let h2 = h;`) makes h2 a param VIEW too — its Drop slot stays
        // unregistered (the caller's fire is the single owner) and the
        // rebind PROPAGATES the view-ness so a later destructure of h2
        // hits the same gates the direct param destructure does. The
        // propagation happens here (the predicate is consulted before
        // `push_drops_for_stmt` on every let) — a query with a mutation
        // is impure but keeps the gate and the propagation at one site.
        if !matches!(
            pattern.kind,
            PatternKind::Struct { .. } | PatternKind::TupleVariant { .. }
        ) {
            if let (PatternKind::Binding(bname), ExprKind::Identifier(src)) =
                (&pattern.kind, &value.kind)
            {
                let src_is_view = self
                    .owned_param_names_stack
                    .last()
                    .is_some_and(|params| params.contains(src.as_str()));
                if src_is_view {
                    let bname = bname.clone();
                    if let Some(top) = self.owned_param_names_stack.last_mut() {
                        top.insert(bname);
                    }
                    return true;
                }
            }
            return false;
        }
        let ExprKind::Identifier(n) = &value.kind else {
            return false;
        };
        self.owned_param_names_stack
            .last()
            .is_some_and(|params| params.contains(n.as_str()))
    }

    pub(crate) fn value_runs_user_drop(&self, value: &Value) -> bool {
        let Value::Struct { name, fields } = value else {
            return false;
        };
        self.program.drop_method_keys.contains_key(name)
            || fields.values().any(|v| self.value_runs_user_drop(v))
    }

    /// Value-level worker for [`Self::drop_user_drop_fields_of_binding`].
    ///
    /// Walks a `Value::Struct`'s fields in REVERSE declaration order (read off
    /// the `StructDef` — the interpreter stores fields in a `HashMap`, whose
    /// iteration order is neither the source order nor even stable run to run,
    /// so drop order has to come from the AST), running each field's own body
    /// before recursing into it.
    ///
    /// `shared struct` fields are SKIPPED: their drop is refcount-driven, so
    /// firing on this holder's death would drop a value other holders still
    /// reference — the same reason the direct-binding path above gates on
    /// `count == 1`. Enum payloads are not walked either; codegen's pass is
    /// struct-field-only, and matching its scope exactly is what keeps the two
    /// backends in step.
    pub(crate) fn drop_user_drop_fields_of_value(&mut self, value: &Value) {
        let Value::Struct {
            name: struct_name,
            fields,
        } = value
        else {
            return;
        };
        let Some(def) = self.find_struct_def(struct_name) else {
            return;
        };
        // (field name, declared head type name). The DECLARED type is what
        // gates the walk — see `declared_field_type_head`.
        let declared: Vec<(String, Option<String>)> = def
            .fields
            .iter()
            .map(|f| (f.name.clone(), Self::declared_field_type_head(&f.ty)))
            .collect();
        for (field, declared_head) in declared.into_iter().rev() {
            let Some(field_value) = fields.get(&field).cloned() else {
                continue;
            };
            let Value::Struct {
                name: field_type, ..
            } = &field_value
            else {
                continue;
            };
            // SCOPING DECISION (B-2026-07-29-39 asked for one explicitly): the
            // walk is DECLARED-type-driven, not value-driven. A field declared
            // as a bare generic param — `struct W[T] { r: T }` instantiated at a
            // Drop type — is skipped, because codegen reads the declared name
            // (`struct_field_type_names`) and simply cannot see through the
            // erasure at the point its glue is emitted. The interpreter, which
            // holds the runtime value, WOULD see it; letting it fire would make
            // `karac run` and `karac build` disagree, and the whole point of the
            // parity gate is that they do not. So both backends skip it and the
            // residual is a LEAK, the safe direction. Comparing the declared
            // head against the runtime struct name is what implements that: they
            // differ exactly when the declared type was erased.
            if declared_head.as_deref() != Some(field_type.as_str()) {
                continue;
            }
            if self.program.drop_method_keys.contains_key(field_type) {
                let field_type = field_type.clone();
                self.run_user_drop_body_only(&field_type, field_value.clone());
            }
            self.drop_user_drop_fields_of_value(&field_value);
        }
    }

    /// Native interpreter `Drop` for `#[compiler_builtin]` stdlib types
    /// whose `impl Drop` releases a side-table resource (held Rust-side
    /// in an interpreter table) rather than running a Kāra body — their
    /// placeholder `fn drop(...) {}` body is a no-op, so the resource
    /// teardown lives here. Returns `true` when `type_name` was handled,
    /// suppressing the no-op body drain. Mirrors codegen's stdlib-drop
    /// special-casing in `src/codegen/synth_drop.rs`
    /// (`emit_hardcoded_stdlib_drop_bodies`: TlsStream / TlsListener /
    /// TaskGroup / …).
    fn try_eval_builtin_drop(&mut self, type_name: &str, name: &str) -> bool {
        match type_name {
            "PooledConnection" => {
                self.drop_pooled_connection(name);
                true
            }
            // `CriticalSectionGuard` (design.md § Critical sections): re-enabling
            // interrupts has no observable effect in a single-threaded tree-walk,
            // so its Drop is inert. Handled here (returning `true`) so the empty
            // `#[compiler_builtin]` drop body is never drained as a user body.
            "CriticalSectionGuard" => true,
            _ => false,
        }
    }

    /// Execute a binding's user `<Type>.drop` body with `self` bound to
    /// the binding's value. Shared by the value-struct and shared-struct
    /// drains in `invoke_user_drop_if_applicable`. No-op when the binding
    /// or the `<Type>.drop` symbol can't be resolved.
    fn run_user_drop_body(&mut self, type_name: &str, name: &str) {
        let value = match self.env.get(name) {
            Some(v) => v,
            None => return,
        };
        self.run_user_drop_body_only(type_name, value);
    }

    /// Value-based core of `run_user_drop_body` — also used by the
    /// fresh-temp call-arg drop hook in `eval_call` (B-2026-07-01-8's
    /// second half: `consume(Guard { id: 7 })` / `consume(Sig.A(1))` had
    /// no binding for the name-keyed runner to resolve).
    ///
    /// B-2026-07-29-39: runs the type's own body and THEN its Drop-bearing
    /// fields' bodies, mirroring codegen's `karac_drop_<T>` wrapper (user body,
    /// then `__karac_dropbodies_<T>`). Every discarded-temp hook in the
    /// interpreter funnels through here, which is what keeps them in step with
    /// codegen's equivalents — those all invoke the wrapper.
    pub(crate) fn run_user_drop_body_on_value(&mut self, type_name: &str, value: Value) {
        self.run_user_drop_body_only(type_name, value.clone());
        self.drop_user_drop_fields_of_value(&value);
    }

    /// The type's OWN `<Type>.drop` body and nothing else. Split out of
    /// [`Self::run_user_drop_body_on_value`] so the recursive field walk can run
    /// a field's body without re-entering the walk for that field (which would
    /// visit every grandchild twice).
    fn run_user_drop_body_only(&mut self, type_name: &str, value: Value) {
        let method_key = format!("{}.drop", type_name);
        let func = match self.env.get(&method_key) {
            Some(f) => f,
            None => return,
        };
        if let Value::Function {
            param_patterns,
            body,
            closure_env,
            ..
        } = func
        {
            self.env.push_scope();
            if let Some(ref captured) = closure_env {
                for (k, v) in captured {
                    self.env.define(k.clone(), v.clone());
                }
            }
            if let Some(self_pat) = param_patterns.first() {
                self.bind_pattern(self_pat, value);
            }
            let _ = self.eval_body_growing(&body);
            self.env.pop_scope();
        }
    }

    /// Sub-slice (3) of move-suppression — interpreter helper that
    /// removes the source binding's Drop slot from `cleanup` when
    /// the block's trailing expression moves out a user-Drop value.
    /// Called immediately before evaluating `block.final_expr` so
    /// the subsequent `run_cleanup` doesn't fire the source's user
    /// body on a value that's already gone to the caller's binding.
    /// Mirrors the codegen `suppress_cleanup_for_tail_return`
    /// behaviour for the user-Drop family.
    fn suppress_tail_expr_user_drop(&mut self, expr: &Expr, cleanup: &mut Vec<CleanupAction>) {
        let name = match &expr.kind {
            ExprKind::Identifier(n) => n.clone(),
            _ => return,
        };
        let type_name = match self.env.get(&name) {
            Some(Value::Struct { name, .. }) => name.clone(),
            // Enum-Drop parity (B-2026-07-01-8): with enum bindings now
            // firing, a moved-out enum binding needs the same suppression
            // or the source AND the destination both run the user body.
            Some(Value::EnumVariant { enum_name, .. }) => enum_name.clone(),
            _ => return,
        };
        if !self.program.drop_method_keys.contains_key(&type_name) {
            return;
        }
        cleanup.retain(|action| match action {
            CleanupAction::Drop { name: drop_name } => drop_name != &name,
            _ => true,
        });
    }

    /// Sub-slice (3) of move-suppression — pre-statement variant for
    /// `return expr;` where expr is an Identifier. Same shape as
    /// `suppress_tail_expr_user_drop` but operates on a `Stmt` (the
    /// outer statement node) so the iteration loop can call it
    /// before dispatching the statement evaluator.
    fn suppress_return_stmt_user_drop(&mut self, stmt: &Stmt, cleanup: &mut Vec<CleanupAction>) {
        let inner_expr = match &stmt.kind {
            StmtKind::Expr(e) => match &e.kind {
                ExprKind::Return(Some(inner)) => inner.as_ref(),
                _ => return,
            },
            _ => return,
        };
        self.suppress_tail_expr_user_drop(inner_expr, cleanup);
    }

    /// Move-suppression for `let g = f;` patterns where `f` is a
    /// binding whose type has a user `impl Drop`. The source `f`'s
    /// CleanupAction::Drop is removed from the current cleanup frame
    /// so its user-body doesn't fire at scope exit (the destination
    /// `g` will fire its drop on the same logical value instead).
    /// No-op when the let statement isn't `Binding = Identifier` or
    /// when the source type isn't user-Drop — non-user-Drop bindings
    /// keep their existing drop_trace records.
    /// B-2026-08-01-8 — `let _ = (r, 20);` moves a Drop-bearing STRUCT
    /// binding into a DISCARDED tuple: no owning destination exists, so the
    /// source's `CleanupAction::Drop` must retract and the discard walk
    /// (`run_discarded_value_user_drops`, whose tuple gate now admits the
    /// place element) becomes the single owner — the codegen twin retracts
    /// via `suppress_user_drop_for_var` and lets the tuple temp's element
    /// walk fire body + free at the `;`. Wildcard-let tuple literals only;
    /// every other shape keeps its existing channels.
    fn suppress_discarded_tuple_moved_elem_user_drops(
        &mut self,
        stmt: &Stmt,
        cleanup: &mut Vec<CleanupAction>,
    ) {
        let StmtKind::Let { pattern, value, .. } = &stmt.kind else {
            return;
        };
        if !matches!(pattern.kind, PatternKind::Wildcard) {
            return;
        }
        let ExprKind::Tuple(elems) = &value.kind else {
            return;
        };
        for e in elems {
            let ExprKind::Identifier(n) = &e.kind else {
                continue;
            };
            let Some(v) = self.env.get(n) else {
                continue;
            };
            if matches!(&v, Value::Struct { .. }) && self.value_runs_user_drop(&v) {
                cleanup
                    .retain(|a| !matches!(a, CleanupAction::Drop { name } if name == n.as_str()));
            }
        }
    }

    fn suppress_let_rebind_user_drop(&mut self, stmt: &Stmt, cleanup: &mut Vec<CleanupAction>) {
        let StmtKind::Let { value, .. } = &stmt.kind else {
            return;
        };
        // B-2026-07-29-38: an AGGREGATE-LITERAL field initializer is a move
        // position too (`ownership_oracle`'s `Role::Move` lists
        // "aggregate-literal field" alongside the direct rebind), so
        // `let h = Holder { r: r };` transfers `r` into `h` exactly as
        // `let g = f;` does. Only the bare-rebind arm existed, so the source
        // kept its Drop slot and the NLL last-use placement fired it AT THE
        // MOVE — for a `TcpListener` field that closes the fd before the
        // listener is ever used. `spread` (`..base`) is deliberately NOT
        // treated as a move source: it copies remaining fields from a base
        // that stays live and owns its own drop.
        let source_names: Vec<String> = match &value.kind {
            ExprKind::Identifier(n) => vec![n.clone()],
            ExprKind::StructLiteral { fields, .. } => fields
                .iter()
                .filter_map(|f| match &f.value.kind {
                    ExprKind::Identifier(n) => Some(n.clone()),
                    _ => None,
                })
                .collect(),
            _ => return,
        };
        for source_name in source_names {
            // Only suppress when the source's value has a user impl Drop.
            let type_name = match self.env.get(&source_name) {
                Some(Value::Struct { name, .. }) => name.clone(),
                // Enum-Drop parity — see `suppress_tail_expr_user_drop`.
                Some(Value::EnumVariant { enum_name, .. }) => enum_name.clone(),
                _ => continue,
            };
            if !self.program.drop_method_keys.contains_key(&type_name) {
                continue;
            }
            cleanup.retain(|action| match action {
                CleanupAction::Drop { name } => name != &source_name,
                _ => true,
            });
        }
    }

    /// B-2026-07-30-11 (displaced-value leg) — the ASSIGN sibling of
    /// [`Self::suppress_let_rebind_user_drop`]: `a = b;` moves `b`'s value
    /// into `a`, so `b`'s own Drop slot must be retracted (the value's body
    /// now fires through `a`, exactly once). Identifier RHS only — any other
    /// RHS constructs a fresh value and moves no binding. Runs with the other
    /// post-statement hooks; `b`'s env slot still holds the (now moved-from)
    /// value, which is all the type lookup needs. The retain is a no-op when
    /// `b`'s Drop slot lives in an outer scope's cleanup vec, the same
    /// (accepted) limitation as the let-rebind hook.
    fn suppress_assign_move_user_drop(&mut self, stmt: &Stmt, cleanup: &mut Vec<CleanupAction>) {
        let StmtKind::Assign { target, value } = &stmt.kind else {
            return;
        };
        let ExprKind::Identifier(source_name) = &value.kind else {
            return;
        };
        // B-2026-08-01-16 — the ASSIGN sibling of the Let-path param-view
        // rebind (B-2026-08-01-15): `h2 = h;` where the RHS is in the current
        // fn's owned-param view set moves the callee's entry copy into `h2`,
        // but under caller-retains the value's Drop observability stays the
        // CALLER's. The displaced old `h2` value's body already ran in the
        // Assign arm; retract the TARGET's Drop slot so its body doesn't fire
        // a second time on the caller-retained value, and propagate view-ness
        // (later rebinds/destructures/matches of `h2` consult the set — the
        // same transitivity the Let path gets in
        // `let_destructures_owned_param`). Codegen twin: the
        // `param_view_locals` insert + `suppress_user_drop_for_var` in the
        // Assign arm of `compile_stmt`.
        if self
            .owned_param_names_stack
            .last()
            .is_some_and(|s| s.contains(source_name.as_str()))
        {
            if let ExprKind::Identifier(target_name) = &target.kind {
                cleanup.retain(|action| match action {
                    CleanupAction::Drop { name } => name != target_name,
                    _ => true,
                });
                if let Some(top) = self.owned_param_names_stack.last_mut() {
                    top.insert(target_name.clone());
                }
                return;
            }
        }
        let type_name = match self.env.get(source_name) {
            Some(Value::Struct { name, .. }) => name,
            Some(Value::EnumVariant { enum_name, .. }) => enum_name,
            _ => return,
        };
        if !self.program.drop_method_keys.contains_key(&type_name) {
            return;
        }
        cleanup.retain(|action| match action {
            CleanupAction::Drop { name } => name != source_name,
            _ => true,
        });
    }

    /// B-2026-07-30-11 (discarded-temp leg) — does a `let _ = <rhs>;` RHS
    /// provably produce an OWNED value this discard site is responsible for?
    /// Struct/tuple literals, enum-variant constructors, calls to
    /// user-declared functions, a moved identifier (whose own Drop slot the
    /// let-rebind hook retracts — the discard fire is then the single body),
    /// and the owning container methods (`insert`/`remove`/`pop*`/`take`,
    /// which return a displaced/extracted value the caller owns). Everything
    /// else — `get`/`first`/`last`/`peek` borrows, unknown methods, arbitrary
    /// expressions — stays silent: uncertain ⇒ silent, and firing a body on a
    /// borrowed view would double it against the real owner. Codegen twin:
    /// the same match in `compile_stmt`'s wildcard-let path; the two must
    /// stay identical or the backends fire on different shapes.
    fn discard_rhs_produces_owned_value(&self, rhs: &Expr, val: &Value) -> bool {
        match &rhs.kind {
            ExprKind::StructLiteral { .. } | ExprKind::Identifier(_) => true,
            // A tuple LITERAL only when every element is itself a fresh
            // temporary or a scalar copy: a heap/Drop-carrying PLACE element
            // (`let _ = (r, 1)`) moves a binding whose own Drop slot stays
            // armed (tuple-literal moves have no retraction hook, unlike the
            // top-level `let _ = r` rebind hook), so a value-walk fire here
            // would double its body. The codegen twin applies the identical
            // rule (scalar-ness there is the element's inferred TypeExpr;
            // here it is the evaluated element value).
            ExprKind::Tuple(elems) => {
                matches!(val, Value::Tuple(items) if self.discard_tuple_all_elems_safe(elems, items, true))
            }
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Path { .. } => true,
                ExprKind::Identifier(n) => self
                    .program
                    .items
                    .iter()
                    .any(|it| matches!(it, Item::Function(f) if &f.name == n)),
                _ => false,
            },
            ExprKind::MethodCall { method, .. } => {
                matches!(
                    method.as_str(),
                    "insert" | "remove" | "pop" | "pop_back" | "pop_front" | "take"
                )
                // B-2026-07-30-11 (user-method discard): `f.make();` — a USER
                // impl method returning an owned struct or value enum
                // produces a fresh value this discard site owns, exactly
                // like the free-fn arm above. Admitted only when the
                // evaluated value is a bare struct/enum whose name matches
                // some impl method `method`'s declared owned (Path, non-ref)
                // return — a borrow-shaped or builtin accessor result never
                // satisfies both (its value is an Option/copy, or no user
                // decl names it), and the builtin borrow names are excluded
                // outright so a user method that shadows them can never fire
                // on the builtin's alias. Enum returns admitted since
                // B-2026-08-01-2. Codegen twin: the MethodCall arm of
                // `try_track_discarded_user_drop_temp` (receiver-type keyed
                // `Type.method` lookup, `user_ref_method_names` excluded).
                || (!matches!(method.as_str(), "get" | "first" | "last" | "peek")
                    && match val {
                        Value::Struct { name, .. } => {
                            self.user_method_returns_owned_type(method, name)
                        }
                        Value::EnumVariant { enum_name, .. } => {
                            self.user_method_returns_owned_type(method, enum_name)
                        }
                        _ => false,
                    })
            }
            _ => false,
        }
    }

    /// True when SOME user `impl` block declares a method `method` whose
    /// return type is a bare owned `Path` naming `type_name` (a struct or a
    /// value enum). The declared-return check is what keeps borrow accessors
    /// out: a `ref self`-borrowing method returning `ref T` has a `Ref`
    /// return kind and never matches.
    fn user_method_returns_owned_type(&self, method: &str, type_name: &str) -> bool {
        self.program.items.iter().any(|it| {
            let Item::ImplBlock(imp) = it else {
                return false;
            };
            imp.items.iter().any(|ii| {
                let crate::ast::ImplItem::Method(f) = ii else {
                    return false;
                };
                f.name == method
                    && f.return_type.as_ref().is_some_and(|te| {
                        matches!(&te.kind, crate::ast::TypeKind::Path(p)
                            if p.segments.last().is_some_and(|s| s == type_name))
                    })
            })
        })
    }

    /// Zip-walk of a discarded tuple literal's element exprs against the
    /// evaluated element values: every element must be FRESH
    /// (`discard_tuple_elem_is_fresh`), a scalar copy (Int/Float/Bool/
    /// Char/Unit — no cleanup can alias it), or a nested tuple that
    /// satisfies the same rule.
    pub(super) fn discard_tuple_all_elems_safe(
        &self,
        elems: &[Expr],
        items: &[Value],
        allow_moved_place: bool,
    ) -> bool {
        if elems.len() != items.len() {
            return false;
        }
        elems.iter().zip(items).all(|(e, v)| {
            if self.discard_tuple_elem_is_fresh(e) {
                return true;
            }
            match (&e.kind, v) {
                (ExprKind::Tuple(ie), Value::Tuple(iv)) => {
                    self.discard_tuple_all_elems_safe(ie, iv, allow_moved_place)
                }
                // B-2026-08-01-8: an Identifier element holding a
                // Drop-bearing STRUCT is admitted — its source binding's
                // Drop action was retracted at the statement
                // (`suppress_discarded_tuple_moved_elem_user_drops`), so
                // the discard walk here is the single owner. Codegen twin:
                // `tuple_elem_is_movable_drop_struct_place`.
                (ExprKind::Identifier(_), Value::Struct { .. }) => {
                    allow_moved_place && self.value_runs_user_drop(v)
                }
                _ => matches!(
                    v,
                    Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Char(_) | Value::Unit
                ),
            }
        })
    }

    /// Tuple-element gate for the discard fire above: is this element a
    /// FRESH value (literal / constructor / owning call), i.e. provably not
    /// a place expression aliasing a live binding? Scalar literals are fresh
    /// (they just carry no Drop work); `Identifier` / field / index / any
    /// unknown shape is not. Mirrors `discard_rhs_produces_owned_value`
    /// minus the top-level-only `Identifier` arm.
    fn discard_tuple_elem_is_fresh(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Integer(..)
            | ExprKind::Float(..)
            | ExprKind::Bool(_)
            | ExprKind::CharLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::InterpolatedStringLit(_)
            | ExprKind::StructLiteral { .. } => true,
            ExprKind::Tuple(elems) => elems.iter().all(|el| self.discard_tuple_elem_is_fresh(el)),
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Path { .. } => true,
                ExprKind::Identifier(n) => self
                    .program
                    .items
                    .iter()
                    .any(|it| matches!(it, Item::Function(f) if &f.name == n)),
                _ => false,
            },
            _ => false,
        }
    }

    /// B-2026-07-30-11 (discarded-temp leg) — run the user Drop work a
    /// discarded owned value carries: a Drop struct's body (+ Drop-bearing
    /// field bodies, via the shared funnel), a tuple's elements, an enum /
    /// Option / Result temp's own body (when declared) and its payloads.
    /// Value-shape recursion mirrors the container-element loop in
    /// `run_array_element_user_drops`.
    pub(super) fn run_discarded_value_user_drops(&mut self, val: Value) {
        match val {
            Value::Struct { ref name, .. } => {
                if self.program.drop_method_keys.contains_key(name) {
                    let tn = name.clone();
                    self.run_user_drop_body_on_value(&tn, val);
                } else if self.value_runs_user_drop(&val) {
                    self.drop_user_drop_fields_of_value(&val);
                }
            }
            Value::Tuple(items) => {
                for e in items {
                    self.run_discarded_value_user_drops(e);
                }
            }
            Value::EnumVariant {
                ref enum_name,
                ref data,
                ..
            } => {
                if enum_name == "Option" || enum_name == "Result" {
                    // Built-ins: value-driven payload recursion. Codegen's
                    // twin is the instantiation-driven optres registrar
                    // (`track_discarded_optres_payload_bodies`), which
                    // resolves the concrete payload type through the same
                    // chain the Let arm records, so both fire on a concrete
                    // instantiation and skip an erased one.
                    match data {
                        EnumData::Unit => {}
                        EnumData::Tuple(vs) => {
                            for v in vs.clone() {
                                self.run_discarded_value_user_drops(v);
                            }
                        }
                        EnumData::Struct(m) => {
                            for v in m.values().cloned().collect::<Vec<_>>() {
                                self.run_discarded_value_user_drops(v);
                            }
                        }
                    }
                } else if self.program.drop_method_keys.contains_key(enum_name) {
                    // Own-`impl Drop` enum: OWN body only. Codegen's discard
                    // registrar hangs the `karac_drop_<E>` wrapper (own body,
                    // no payload walk) on the frame for this shape, and the
                    // sibling legs (enum-assign displacement) exclude
                    // own-Drop enums from the payload walk the same way —
                    // firing payloads here would print bodies `karac build`
                    // does not (B-2026-08-01-2 probe p3).
                    let tn = enum_name.clone();
                    self.run_user_drop_body_on_value(&tn, val.clone());
                } else {
                    // User value enum: declared-type-driven payload walk,
                    // the exact shape codegen's `__karac_dropelems_enum_<E>`
                    // admits (B-2026-08-01-2). The previous value-driven
                    // recursion here fired on erased-generic payloads that
                    // codegen structurally cannot see (probe p5) — the walk
                    // keeps both backends silent on those, the safe
                    // direction.
                    self.run_enum_payload_user_drops_value(&val);
                }
            }
            _ => {}
        }
    }

    /// Move-suppression for `forget(x);` statements (design.md § Exported
    /// C ABI, Slice 4). `forget` consumes its argument and suppresses the
    /// destructor; the tree-walk analogue is to remove the source
    /// binding's `CleanupAction::Drop` so its user body never fires at
    /// scope exit — the value is handed off (leaked from Kāra's view), the
    /// FFI ownership-handoff contract. Sibling of
    /// [`suppress_let_rebind_user_drop`]; runs after the statement
    /// evaluates (the consume has happened). No-op unless the statement is
    /// `forget(<ident>)` and the source's type has a user `impl Drop`.
    fn suppress_forget_stmt_user_drop(&mut self, stmt: &Stmt, cleanup: &mut Vec<CleanupAction>) {
        let call = match &stmt.kind {
            StmtKind::Expr(e) => e,
            _ => return,
        };
        let (callee, args) = match &call.kind {
            ExprKind::Call { callee, args, .. } => (callee, args),
            _ => return,
        };
        if !matches!(&callee.kind, ExprKind::Identifier(n) if n == "forget") {
            return;
        }
        let source_name = match args.first().map(|a| &a.value.kind) {
            Some(ExprKind::Identifier(n)) => n.clone(),
            _ => return,
        };
        let type_name = match self.env.get(&source_name) {
            Some(Value::Struct { name, .. }) => name.clone(),
            Some(Value::EnumVariant { enum_name, .. }) => enum_name.clone(),
            _ => return,
        };
        if !self.program.drop_method_keys.contains_key(&type_name) {
            return;
        }
        cleanup.retain(|action| match action {
            CleanupAction::Drop { name } => name != &source_name,
            _ => true,
        });
    }

    /// Whole-value container move-out recorder — the interpreter twin of
    /// codegen's `disarm_container_bodies_move_sources`. For each bare
    /// identifier the RHS moves wholesale (direct rebind, struct-literal
    /// field, tuple-literal element), record the source so its
    /// element/payload-body walks skip at drop time. A struct whose FIELDS
    /// carry the drop routes to the existing `moved_out_drop_field_bindings`
    /// set (the field walk's own disarm channel); a struct with its own
    /// `impl Drop` is left to `suppress_let_rebind_user_drop`, which already
    /// retracts its whole action.
    fn record_container_bodies_move_sources(&mut self, value: &Expr) {
        match &value.kind {
            ExprKind::Identifier(n) => self.record_container_move_source_name(n),
            ExprKind::SelfValue => self.record_container_move_source_name("self"),
            ExprKind::StructLiteral { fields, .. } => {
                for f in fields {
                    if let ExprKind::Identifier(n) = &f.value.kind {
                        self.record_container_move_source_name(n);
                    }
                }
            }
            ExprKind::Tuple(elems) => {
                for e in elems {
                    if let ExprKind::Identifier(n) = &e.kind {
                        self.record_container_move_source_name(n);
                    }
                }
            }
            _ => {}
        }
    }

    /// Single-name worker for [`Self::record_container_bodies_move_sources`],
    /// also called at consuming method-arg sites (`v.push(e)`). Value-shape
    /// gated so direct struct bindings keep their existing (balanced) move
    /// machinery.
    pub(crate) fn record_container_move_source_name(&mut self, name: &str) {
        match self.env.get(name) {
            Some(Value::EnumVariant { .. } | Value::Array(_) | Value::Tuple(_) | Value::Map(_)) => {
                self.moved_out_container_bodies_bindings
                    .insert(name.to_string());
            }
            Some(v @ Value::Struct { .. }) => {
                // Field-carried drop only: the own-`Drop` struct is handled by
                // `suppress_let_rebind_user_drop`'s action retraction.
                let own_drop = matches!(&v, Value::Struct { name: tn, .. }
                    if self.program.drop_method_keys.contains_key(tn));
                if !own_drop && self.value_runs_user_drop(&v) {
                    self.moved_out_drop_field_bindings.insert(name.to_string());
                }
            }
            _ => {}
        }
    }

    /// Re-arm a name that just received a FRESH value (a new `let` binding or
    /// an assignment target): stale move-out records from a previous binding
    /// of the same name must not silence the new value's walks.
    fn rearm_container_bodies_for_name(&mut self, name: &str) {
        self.moved_out_container_bodies_bindings.remove(name);
        self.moved_out_drop_field_bindings.remove(name);
        self.moved_out_enum_payload_bindings.remove(name);
        self.moved_out_user_drop_bindings.remove(name);
    }

    /// B-2026-07-30-11 (Option/Result leg) — a bare-identifier payload arg of
    /// a VARIANT CONSTRUCTOR (`Ok(h)`, `Some(h)`, `Slot.Held(r)`) moves the
    /// whole binding into the variant: silence every drop the source binding
    /// would run (own body and container walks alike) — the enum's owner runs
    /// them now. Codegen's twin is the `suppress_user_drop_for_var` call in
    /// `try_compile_enum_variant`'s arg loop. Deliberately NOT applied to
    /// ordinary fn-call args: those follow the caller-drops convention on
    /// both backends (`run_fresh_temp_arg_drops` excludes identifier args for
    /// the same reason).
    pub(crate) fn record_ctor_arg_moves(&mut self, args: &[crate::ast::CallArg]) {
        for arg in args {
            if let ExprKind::Identifier(n) = &arg.value.kind {
                let runs = match self.env.get(n) {
                    Some(v @ Value::Struct { .. }) => self.value_runs_user_drop(&v),
                    Some(Value::EnumVariant { .. } | Value::Array(_) | Value::Tuple(_)) => true,
                    _ => false,
                };
                if runs {
                    self.moved_out_user_drop_bindings.insert(n.clone());
                }
            }
        }
    }

    /// B-2026-07-30-11 (Option/Result leg) — record the binding's resolved
    /// `Option[P]` / `Result[O, E]` instantiation for the payload-bodies
    /// walk. The resolution chain MIRRORS codegen's registration verbatim
    /// (annotation → span-keyed `enum_inst_type_exprs` → callee's declared
    /// return type → the source var's record for a bare rebind); a te whose
    /// payload head names no user struct — including a bare generic param in
    /// an unmonomorphized body — fails the gate on BOTH backends, so the
    /// erased-generic residual is a shared leak rather than a divergence.
    /// A let that does NOT qualify removes any stale record for the name.
    fn record_optres_payload_te(&mut self, name: &str, ty: &Option<TypeExpr>, value: &Expr) {
        // Borrow-returning accessors are EXCLUDED — `v.get(i)` / `.first()` /
        // `.last()` yield an Option whose payload aliases the container's
        // element, whose own walk runs the body. Mirrors codegen's skip at
        // the let-site registration.
        if matches!(
            &value.kind,
            ExprKind::MethodCall { method, .. }
                if matches!(method.as_str(), "get" | "first" | "last")
        ) {
            self.optres_payload_bodies_tes.remove(name);
            return;
        }
        let te = ty
            .clone()
            .or_else(|| {
                self.program
                    .enum_inst_type_exprs
                    .get(&(value.span.offset, value.span.length))
                    .cloned()
            })
            .or_else(|| match &value.kind {
                ExprKind::Call { callee, .. } => match &callee.kind {
                    ExprKind::Identifier(f) => {
                        self.program.items.iter().find_map(|item| match item {
                            Item::Function(func) if func.name == *f => func.return_type.clone(),
                            _ => None,
                        })
                    }
                    _ => None,
                },
                _ => None,
            })
            .or_else(|| match &value.kind {
                ExprKind::Identifier(n) => self.optres_payload_bodies_tes.get(n).cloned(),
                _ => None,
            });
        let qualifies = te.as_ref().is_some_and(|te| {
            let TypeKind::Path(p) = &te.kind else {
                return false;
            };
            let head = p.segments.last().map(String::as_str);
            let Some(args) = p.generic_args.as_ref() else {
                return false;
            };
            let payload_tes: Vec<&TypeExpr> = args
                .iter()
                .filter_map(|a| match a {
                    crate::ast::GenericArg::Type(t) => Some(t),
                    _ => None,
                })
                .collect();
            match head {
                Some("Option") | Some("Result") => payload_tes
                    .iter()
                    .any(|pt| self.type_expr_runs_user_drop(pt)),
                _ => false,
            }
        });
        if qualifies {
            self.optres_payload_bodies_tes
                .insert(name.to_string(), te.expect("qualifies implies Some"));
        } else {
            self.optres_payload_bodies_tes.remove(name);
        }
    }

    /// B-2026-07-30-11 (Map-values leg) — record the binding's resolved
    /// `Map[K, V]` instantiation for the value-bodies walk. Chain mirrors
    /// codegen's registration verbatim: annotation → bare-identifier
    /// callee's declared return → the source var's record for a bare rebind.
    /// A non-qualifying let clears any stale record.
    fn record_map_val_bodies_te(&mut self, name: &str, ty: &Option<TypeExpr>, value: &Expr) {
        let te = ty
            .clone()
            .or_else(|| match &value.kind {
                ExprKind::Call { callee, .. } => match &callee.kind {
                    ExprKind::Identifier(f) => {
                        self.program.items.iter().find_map(|item| match item {
                            Item::Function(func) if func.name == *f => func.return_type.clone(),
                            _ => None,
                        })
                    }
                    _ => None,
                },
                _ => None,
            })
            .or_else(|| match &value.kind {
                ExprKind::Identifier(n) => self.map_val_bodies_tes.get(n).cloned(),
                _ => None,
            });
        let qualifies = te.as_ref().is_some_and(|te| {
            let TypeKind::Path(p) = &te.kind else {
                return false;
            };
            // Map/SortedMap gate on the VALUE arg; Set/SortedSet on the
            // ELEMENT arg (B-2026-07-30-11 Set-elements leg — a Set lowers
            // to the key half of the same table, so the walk is the
            // key-side sibling of the values walk).
            let elem_idx = match p.segments.last().map(String::as_str) {
                Some("Map") | Some("SortedMap") => 1usize,
                Some("Set") | Some("SortedSet") => 0usize,
                _ => return false,
            };
            matches!(
                p.generic_args.as_ref().and_then(|a| a.get(elem_idx)),
                Some(crate::ast::GenericArg::Type(v)) if self.type_expr_runs_user_drop(v)
            )
        });
        if qualifies {
            self.map_val_bodies_tes
                .insert(name.to_string(), te.expect("qualifies implies Some"));
        } else {
            self.map_val_bodies_tes.remove(name);
        }
    }

    /// The walk half: run each stored VALUE's user `impl Drop` body for a
    /// dying `Map` binding. Insertion order — codegen's walk is bucket
    /// order, and the two differ exactly as `for (k, v) in m` already does
    /// (unordered-map semantics); parity tests are order-insensitive.
    /// Returns `true` when the binding resolved to a Map (walked or not) so
    /// the caller stops — a Map is not a `drop_target` shape.
    fn run_map_val_user_drops(&mut self, name: &str) -> bool {
        // SortedMap shares the walk (same declared-V gate); its values come
        // out in key order vs the Map's insertion order — the same
        // ordered-vs-unordered difference `for (k, v) in m` already has, so
        // parity tests stay order-insensitive.
        let vals: Vec<Value> = match self.env.get(name) {
            Some(Value::Map(entries)) => entries.into_iter().map(|(_, v)| v).collect(),
            Some(Value::SortedMap(entries)) => entries.into_values().collect(),
            // Set-elements leg (B-2026-07-30-11): the walked values are the
            // ELEMENTS — the key half of the same table shape.
            Some(Value::Set(items)) => items,
            Some(Value::SortedSet(items)) => items.into_keys().map(|k| k.0).collect(),
            _ => return false,
        };
        if self.moved_out_container_bodies_bindings.contains(name) {
            return true;
        }
        let Some(te) = self.map_val_bodies_tes.get(name).cloned() else {
            return true;
        };
        let TypeKind::Path(p) = &te.kind else {
            return true;
        };
        let elem_idx = match p.segments.last().map(String::as_str) {
            Some("Set") | Some("SortedSet") => 0usize,
            _ => 1usize,
        };
        let Some(crate::ast::GenericArg::Type(val_te)) =
            p.generic_args.as_ref().and_then(|a| a.get(elem_idx))
        else {
            return true;
        };
        let declared_head = Self::declared_field_type_head(val_te);
        for v in vals {
            let Value::Struct { name: tn, .. } = &v else {
                continue;
            };
            if declared_head.as_deref() != Some(tn.as_str()) {
                continue;
            }
            let tn = tn.clone();
            if self.program.drop_method_keys.contains_key(&tn) {
                self.run_user_drop_body_on_value(&tn, v);
            } else if self.value_runs_user_drop(&v) {
                self.drop_user_drop_fields_of_value(&v);
            }
        }
        true
    }

    /// The walk half of [`Self::record_optres_payload_te`]: run the live
    /// payload's user `impl Drop` body (and its field bodies) for a dying
    /// `Option`/`Result` binding. BODY ONLY — the interpreter's value model
    /// owns the memory. Declared-head-vs-runtime-name gated exactly like the
    /// struct-field walk, so a payload whose static type was erased at the
    /// point codegen emitted its walker is skipped here too.
    fn run_optres_payload_user_drops(&mut self, name: &str, variant: &str, data: &EnumData) {
        let Some(te) = self.optres_payload_bodies_tes.get(name).cloned() else {
            return;
        };
        let TypeKind::Path(p) = &te.kind else {
            return;
        };
        let Some(args) = p.generic_args.as_ref() else {
            return;
        };
        let payload_pos = match (p.segments.last().map(String::as_str), variant) {
            (Some("Option"), "Some") | (Some("Result"), "Ok") => 0usize,
            (Some("Result"), "Err") => 1usize,
            _ => return,
        };
        let Some(crate::ast::GenericArg::Type(payload_te)) = args.get(payload_pos) else {
            return;
        };
        let declared_head = Self::declared_field_type_head(payload_te);
        let EnumData::Tuple(items) = data else {
            return;
        };
        let Some(payload) = items.first() else {
            return;
        };
        let Value::Struct { name: tn, .. } = payload else {
            return;
        };
        if declared_head.as_deref() != Some(tn.as_str()) {
            return;
        }
        let tn = tn.clone();
        let payload = payload.clone();
        if self.program.drop_method_keys.contains_key(&tn) {
            self.run_user_drop_body_only(&tn, payload.clone());
        }
        self.drop_user_drop_fields_of_value(&payload);
    }

    #[allow(clippy::result_large_err)]
    fn eval_stmt_cf(&mut self, stmt: &Stmt) -> EvalResult {
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let {
                pattern, ty, value, ..
            } => {
                // Thread the binding's `Tensor[Elem, …]` annotation (when
                // present) into a fill-type hint for any `Tensor.zeros` /
                // `Tensor.ones` in the RHS — the only place the concrete
                // element type `T` survives for the dynamically-typed
                // interpreter (see `tensor_scalar_fill`). Saved/restored so
                // nested `let`s in a block-expr RHS nest correctly.
                let saved_tensor_fill = self.pending_tensor_fill;
                self.pending_tensor_fill = ty
                    .as_ref()
                    .and_then(super::method_call_tensor::tensor_elem_fill);
                // B-2026-07-28-10: the same channel, carrying the whole
                // annotation, for constructors whose element type is not
                // recoverable from their argument (`Column.from_arrow_ipc`
                // takes an opaque byte stream).
                let saved_let_ty = self.pending_let_ty.take();
                self.pending_let_ty = ty.clone();
                // REPL value-snapshot replay: when the binding pattern is
                // a single `Binding(..)` and the binder pattern's span is
                // in `let_value_overrides`, skip RHS evaluation entirely
                // and use the pre-loaded value. This is what makes `let x
                // = read_file("…");` from cell N stop re-reading the file
                // when cell N+1's source-replay reintroduces the same
                // `let`. Span keying restricts the short-circuit to the
                // binder the REPL selected (the LAST binder of each name
                // — earlier shadows re-run their true RHS in order; see
                // `Session::install_let_snapshot_overrides`). Pattern
                // lets fall through the normal path.
                let val = if let crate::ast::PatternKind::Binding(_) = &pattern.kind {
                    if let Some(snapshot) = self
                        .let_value_overrides
                        .get(&crate::resolver::SpanKey::from_span(&pattern.span))
                    {
                        snapshot.clone()
                    } else {
                        let v = self.eval_expr_inner(value);
                        if let Some(cf) = self.pending_cf.take() {
                            self.pending_tensor_fill = saved_tensor_fill;
                            self.pending_let_ty = saved_let_ty;
                            return Err(cf);
                        }
                        v
                    }
                } else {
                    let v = self.eval_expr_inner(value);
                    if let Some(cf) = self.pending_cf.take() {
                        self.pending_tensor_fill = saved_tensor_fill;
                        self.pending_let_ty = saved_let_ty;
                        return Err(cf);
                    }
                    v
                };
                self.pending_tensor_fill = saved_tensor_fill;
                self.pending_let_ty = saved_let_ty;
                // NO snapshot capture here. It used to fire at this point,
                // freezing each watched binding at its INITIALIZER, so any
                // mutation later in the same REPL cell was lost crossing to
                // the next one (B-2026-07-29-20): `let mut n: i64 = 0; n =
                // 5;` read back 0, and `let mut m = Map.new(); m.insert(…)`
                // read back empty. `Vec` was the one row that appeared to
                // work, and only by accident — `Value::Array` is an
                // `Arc<RwLock<…>>`, so the clone taken here aliases the same
                // storage and observes later pushes. `Map(Vec<(Value,
                // Value)>)` and `Set(Vec<Value>)` are by-value, so their
                // clones froze. Capture now happens once at end of `main`
                // (`call_function`), which is uniform across all three.
                //
                // Whole-value container moves out of the RHS (`let b = a;`,
                // `Box2 { s: d }`, `(h, 1)`): silence the sources' walks
                // before binding, then re-arm the freshly-bound names so a
                // stale record from a prior same-named binding can't silence
                // the new one.
                self.record_container_bodies_move_sources(value);
                // B-2026-07-30-11 (discarded-temp leg): `let _ = <owned>;`
                // throws the value away with no binding, so no Drop slot ever
                // ran its user Drop work — a Drop struct literal, a tuple
                // carrying Drop elements, an Option/Result temp with a Drop
                // payload (the discarded displacing-insert return) were all
                // silent. Fire here, at the discard point, gated on the
                // ownership shape of the RHS. Fired before `bind_pattern`
                // consumes `val`; for an Identifier RHS the let-rebind hook
                // retracts the source's own slot right after this statement,
                // so the body still runs exactly once.
                if matches!(pattern.kind, crate::ast::PatternKind::Wildcard)
                    && self.discard_rhs_produces_owned_value(value, &val)
                {
                    self.run_discarded_value_user_drops(val.clone());
                }
                self.bind_pattern(pattern, val);
                for bound in pattern.binding_names() {
                    self.rearm_container_bodies_for_name(&bound);
                }
                // B-2026-07-30-11 (Option/Result leg): record (or clear) the
                // binding's payload-bodies te — the registration moment,
                // mirroring codegen's let-site walker registration.
                if let crate::ast::PatternKind::Binding(bname) = &pattern.kind {
                    let bname = bname.clone();
                    self.record_optres_payload_te(&bname, ty, value);
                    // Map-values leg: same registration moment, same chain.
                    self.record_map_val_bodies_te(&bname, ty, value);
                }
            }
            StmtKind::LetUninit { name, .. } => {
                // Declare the binding with a sentinel `Unit` value. Static
                // definite-assignment analysis (in `OwnershipChecker`)
                // rejects any read before the first assignment, so a
                // well-typed program never observes this sentinel.
                self.env.define(name.clone(), Value::Unit);
            }
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                let val = self.eval_expr_inner(value);
                // Poison discipline (B-2026-07-31-15): a faulted scrutinee must
                // propagate — without this the poison Unit falls into
                // `try_match_pattern`, misses, and runs the `else` block
                // spuriously on an error path that should unwind.
                if let Some(cf) = self.pending_cf.take() {
                    return Err(cf);
                }
                // B-2026-07-11-26: run a fresh-temp enum scrutinee's user `Drop`
                // (both edges), mirroring codegen — on the miss edge AFTER the
                // else block runs (matching codegen's drop-during-return order).
                let scrut_drop = self.freshtemp_scrutinee_user_drop_type(value);
                let drop_val = scrut_drop.as_ref().map(|_| val.clone());
                // B-2026-07-31-45 — the `let…else` twin of the match/if-let
                // disarm: this pattern moves a Drop-bearing payload out into
                // an ESCAPING binding, so the source's payload-body walk must
                // skip it or the body runs twice (once via the source's walk
                // at its NLL death — before the binding is even used — and
                // once via the binding's own slot).
                self.disarm_moved_out_enum_payload_one(value, &val, pattern);
                if self.try_match_pattern(pattern, &val) {
                    self.bind_pattern(pattern, val);
                    if let (Some(tn), Some(dv)) = (scrut_drop, drop_val) {
                        self.run_user_drop_body_on_value(&tn, dv);
                    }
                } else {
                    let else_result = self.eval_block_inner(else_block);
                    if let (Some(tn), Some(dv)) = (scrut_drop, drop_val) {
                        self.run_user_drop_body_on_value(&tn, dv);
                    }
                    else_result?;
                }
            }
            StmtKind::Defer { body } => {
                // Collect for later execution — we'll run these when we have
                // a proper scope-exit mechanism. For now, run inline as a
                // simplified approximation.
                let _ = body;
            }
            StmtKind::ErrDefer { body, .. } => {
                let _ = body;
            }
            StmtKind::Assign { target, value } => {
                // `f = g;` — g's container value moves into f. Record g so
                // its walks skip (codegen's Assign-arm disarm twin), and
                // re-arm f below once the store lands.
                self.record_container_bodies_move_sources(value);
                let val = self.eval_expr_inner(value);
                // A faulted RHS (index OOB, unwrap of None, …) or a control-flow
                // signal escaping a closure body (`break` out of an enclosing
                // loop through a `with_provider` body, B-2026-07-31-15) sets
                // `pending_cf` and yields a poison value. Propagate the signal
                // instead of storing the poison into the target — `Let` and
                // `Expr` statements already do; storing first corrupted the
                // binding (an i64 accumulator became Unit) before the loop
                // machinery ever saw the `break`.
                if let Some(cf) = self.pending_cf.take() {
                    return Err(cf);
                }
                // B-2026-07-30-11 (displaced-value leg): overwriting a binding
                // whose CURRENT value carries user `impl Drop` work silently
                // discarded that work — `x = Res { .. }` never ran the old
                // value's body (the binding's own NLL fire reads the slot
                // AFTER the store, so it covers the new value). Run the old
                // value's body + Drop-bearing field bodies here, before the
                // store, exactly the container-element treatment. Guards:
                // identifier targets only (field/index displacement has its
                // own machinery), a value already moved out runs nothing, and
                // an RHS that mentions the target at all is skipped —
                // `x = consume(x)` hands the old value to the callee, whose
                // own drop discipline covers it (uncertain ⇒ silent, which is
                // today's behavior, never a double body).
                if let ExprKind::Identifier(t) = &target.kind {
                    if !self.moved_out_user_drop_bindings.contains(t.as_str())
                        && !crate::deque_head::expr_mentions_name_deep(value, t)
                    {
                        if let Some(old) = self.env.get(t) {
                            match &old {
                                Value::Struct { name: tn, .. } => {
                                    if self.program.drop_method_keys.contains_key(tn) {
                                        let tn = tn.clone();
                                        self.run_user_drop_body_on_value(&tn, old);
                                    } else if self.value_runs_user_drop(&old) {
                                        self.drop_user_drop_fields_of_value(&old);
                                    }
                                }
                                // Enum sibling (B-2026-07-30-11, enum-assign
                                // displacement): `b = Box2.Empty;` over
                                // `Full(Res{..})` silently discarded the
                                // payload's Drop work — the Struct-only match
                                // above never saw the EnumVariant. Reuse the
                                // scope-exit payload walk (same moved-out
                                // guards, same forward order as codegen's
                                // `__karac_dropelems_enum_<E>`). An enum with
                                // its OWN `impl Drop` fires that body FIRST,
                                // then the payload walk — the struct twin's
                                // own-body-then-members order (settled by the
                                // own-Drop-enum reassign leg; previously
                                // excluded as unsettled and the old value's
                                // work was silently lost on both backends).
                                // The own body respects the same disarm sets
                                // the payload walk checks internally: a
                                // moved-out value runs nothing.
                                Value::EnumVariant { enum_name, .. } => {
                                    let t = t.clone();
                                    let old = old.clone();
                                    if !self.moved_out_enum_payload_bindings.contains(&t)
                                        && !self.moved_out_container_bodies_bindings.contains(&t)
                                        && self.program.drop_method_keys.contains_key(enum_name)
                                    {
                                        let tn = enum_name.clone();
                                        self.run_user_drop_body_on_value(&tn, old.clone());
                                    }
                                    self.run_enum_payload_user_drops(&t, &old);
                                }
                                _ => {}
                            }
                        }
                    }
                }
                if !self.assign_to_place(target, val) {
                    unreachable!(
                        "unsupported assignment target at {}:{}; should be caught by parser/typechecker",
                        stmt.span.line, stmt.span.column
                    );
                }
                // The target holds a fresh value now — a stale move-out
                // record from its previous value must not silence it.
                if let ExprKind::Identifier(t) = &target.kind {
                    let t = t.clone();
                    self.rearm_container_bodies_for_name(&t);
                }
            }
            StmtKind::CompoundAssign { target, op, value } => {
                let current = self.eval_expr_inner(target);
                // Same poison discipline as `Assign` (B-2026-07-31-15): a
                // faulted operand must propagate, not feed `eval_binary`
                // (whose variant match would hit an internal unreachable on
                // the poison Unit) and then overwrite the target.
                if let Some(cf) = self.pending_cf.take() {
                    return Err(cf);
                }
                let rhs = self.eval_expr_inner(value);
                if let Some(cf) = self.pending_cf.take() {
                    return Err(cf);
                }
                let bin_op = match op {
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
                // Q4 literal promotion (B-2026-07-04-12): `x += 1` with
                // `x: f64` — the `1` promotes to `f64` under check + codegen, so
                // the interpreter must too, or `run` errors on `(Float, Int)`.
                let (current, rhs) =
                    self.promote_int_literal_for_float_peer(&bin_op, target, value, current, rhs);
                // Unsigned-64 compound assignment (`x >>= n`, `x /= n`, `x %= n`
                // on `u64` / `usize`): the target's span carries the u64 type, so
                // thread it as the hint (`stmt.span`'s recorded type may be Unit).
                // B-2026-07-04-8.
                let unsigned_hint = self.span_type_is_unsigned64(&target.span);
                let result = self.eval_binary(&bin_op, current, rhs, &stmt.span, unsigned_hint);
                // Route through `assign_to_place` so compound assignment works
                // on field / index / nested targets (`o.count += 1`,
                // `v[i].x += 1`), not just bare bindings. Previously only the
                // `Identifier` target was handled — field/index compound
                // assigns were silently dropped.
                if !self.assign_to_place(target, result) {
                    unreachable!(
                        "unsupported compound-assignment target at {}:{}; should be caught by parser/typechecker",
                        stmt.span.line, stmt.span.column
                    );
                }
            }
            StmtKind::Expr(expr) => {
                let discarded = self.eval_expr_inner(expr);
                // If a control flow signal was set during expression evaluation,
                // propagate it immediately
                if let Some(cf) = self.pending_cf.take() {
                    return Err(cf);
                }
                // B-2026-07-01-7 (discard position): `make();` where the
                // callee's declared return type has a user `impl Drop` —
                // the discarded temp's body must fire (codegen twin:
                // `try_track_discarded_user_drop_temp`).
                match &expr.kind {
                    ExprKind::Call { callee, .. } => {
                        // Bare Path-callee CTOR discard (`Option.Some(mk());`,
                        // `Sig.A(g);`): the wildcard-let gate admits Path
                        // ctors unconditionally, and codegen's bare arm now
                        // registers the same optres/enum payload bodies —
                        // route through the shared discard walker so both
                        // statement shapes agree (B-2026-07-30-11, optres
                        // bare leg).
                        if matches!(&callee.kind, ExprKind::Path { .. }) {
                            self.run_discarded_value_user_drops(discarded);
                        } else if let ExprKind::Identifier(fn_name) = &callee.kind {
                            if let Some(tn) = self.user_fn_return_type_name(fn_name) {
                                if self.program.drop_method_keys.contains_key(&tn) {
                                    self.run_user_drop_body_on_value(&tn, discarded);
                                } else if self.value_runs_user_drop(&discarded) {
                                    // B-2026-07-30-11 SHAPE 2, discard position —
                                    // the return type declares no `Drop` of its own
                                    // but carries a Drop-bearing field, so `make();`
                                    // leaked that field's resource. Bodies only, as
                                    // in the arg-temp twin.
                                    self.drop_user_drop_fields_of_value(&discarded);
                                } else if matches!(&discarded, Value::EnumVariant { .. }) {
                                    // B-2026-08-01-2 — a discarded user-ENUM
                                    // return (`mk_enum();`): the wildcard-let
                                    // walker, whose EnumVariant arm takes the
                                    // declared-type-driven walk for user enums
                                    // and the value-driven recursion for
                                    // Option/Result (`mkopt(2);` — the bare
                                    // sibling of the optres wildcard-let leg;
                                    // codegen twin: the bare-statement arm now
                                    // calls track_discarded_optres_payload_
                                    // bodies like the `let _ =` arm).
                                    self.run_discarded_value_user_drops(discarded);
                                }
                            }
                        }
                    }
                    // B-2026-07-30-11 (user-method discard): `f.make();` — the
                    // METHOD sibling of the free-fn arm above, admitted under
                    // the same declared-owned-return rule the wildcard-let
                    // gate uses (`user_method_returns_owned_struct`; builtin
                    // borrow names excluded outright). Codegen twin: the
                    // MethodCall arm of `try_track_discarded_user_drop_temp`.
                    ExprKind::MethodCall { method, .. }
                        if !matches!(method.as_str(), "get" | "first" | "last" | "peek") =>
                    {
                        match &discarded {
                            Value::Struct { name, .. } => {
                                let tn = name.clone();
                                if self.user_method_returns_owned_type(method, &tn) {
                                    if self.program.drop_method_keys.contains_key(&tn) {
                                        self.run_user_drop_body_on_value(&tn, discarded);
                                    } else if self.value_runs_user_drop(&discarded) {
                                        self.drop_user_drop_fields_of_value(&discarded);
                                    }
                                }
                            }
                            // B-2026-08-01-2 — the enum sibling: a user
                            // method returning a value enum by declared
                            // owned return (`f.make();`). Own-Drop enums
                            // run their own body (codegen hangs the
                            // `karac_drop_<E>` wrapper); others take the
                            // declared-type-driven payload walk.
                            Value::EnumVariant { enum_name, .. } => {
                                let tn = enum_name.clone();
                                if self.user_method_returns_owned_type(method, &tn) {
                                    if self.program.drop_method_keys.contains_key(&tn) {
                                        self.run_user_drop_body_on_value(&tn, discarded);
                                    } else {
                                        self.run_enum_payload_user_drops_value(&discarded);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(Value::Unit)
    }

    // ── Call evaluation ─────────────────────────────────────────

    /// Execute a lowered primitive operator call (e.g. `i64.add(a, b)`).
    /// Returns `Some(value)` if the method matches a known intrinsic; `None`
    /// otherwise (caller falls through to other dispatch).
    pub(crate) fn dispatch_lowered_op(
        &mut self,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Option<Value> {
        // Map lowered method name back to the corresponding BinOp / UnaryOp
        // and synthesize a Binary/Unary expression that eval_binary/eval_unary
        // already knows how to execute. Reuses all existing intrinsic logic
        // (overflow trapping, division by zero, string concat, etc.).
        let bin_op = match method {
            "add" => Some(BinOp::Add),
            "sub" => Some(BinOp::Sub),
            "mul" => Some(BinOp::Mul),
            "div" => Some(BinOp::Div),
            "rem" => Some(BinOp::Mod),
            "eq" => Some(BinOp::Eq),
            "ne" => Some(BinOp::NotEq),
            "lt" => Some(BinOp::Lt),
            "le" => Some(BinOp::LtEq),
            "gt" => Some(BinOp::Gt),
            "ge" => Some(BinOp::GtEq),
            "bitand" => Some(BinOp::BitAnd),
            "bitor" => Some(BinOp::BitOr),
            "bitxor" => Some(BinOp::BitXor),
            "shl" => Some(BinOp::Shl),
            "shr" => Some(BinOp::Shr),
            _ => None,
        };
        if let Some(op) = bin_op {
            if args.len() == 2 {
                let lhs = self.eval_expr_inner(&args[0].value);
                // A faulted lhs (integer overflow, div-by-zero, index OOB,
                // unwrap of None, …) sets `pending_cf` and yields a poison
                // `Unit`; propagate it immediately. Without this guard the
                // `rhs` eval short-circuits to `Unit` via `check_cf`, and
                // `eval_binary(op, Unit, Unit)` then records a SPURIOUS second
                // "operator not defined for Unit and Unit" diagnostic on top
                // of the real fault (B-2026-07-15-7). This lowered-operator
                // path is where scalar binops reach the interpreter after
                // `karac::lower` rewrites `a + b` into `<type>.add(a, b)`, so
                // it needs the same per-operand short-circuit the
                // `ExprKind::Binary` arm already performs.
                if self.pending_cf.is_some() {
                    return Some(lhs);
                }
                let rhs = self.eval_expr_inner(&args[1].value);
                if self.pending_cf.is_some() {
                    return Some(rhs);
                }
                // Q4 literal promotion (B-2026-07-04-12): the operator lowering
                // rewrites `a + 1` into `<type>.add(a, 1)`, so scalar binops
                // reach the interpreter HERE, not via the `ExprKind::Binary`
                // arm. Apply the same int-literal→float promotion so `a + 1`
                // with `a: f64` matches check + codegen (which lower the `1` as
                // `1.0`) instead of erroring on a `(Float, Int)` pair.
                let (lhs, rhs) = self.promote_int_literal_for_float_peer(
                    &op,
                    &args[0].value,
                    &args[1].value,
                    lhs,
                    rhs,
                );
                // Operand-derived u64 hint (B-2026-07-04-8): comparisons lowered
                // to `u64.lt(a, b)` type this call's result as `bool`, so recover
                // operand signedness from the argument spans.
                let unsigned_hint = self.span_type_is_unsigned64(&args[0].value.span)
                    || self.span_type_is_unsigned64(&args[1].value.span);
                return Some(self.eval_binary(&op, lhs, rhs, span, unsigned_hint));
            }
        }
        if method == "neg" && args.len() == 1 {
            let val = self.eval_expr_inner(&args[0].value);
            // Same fault short-circuit as the binary arm — a faulted operand
            // must not reach `eval_unary` on its `Unit` poison (B-2026-07-15-7).
            if self.pending_cf.is_some() {
                return Some(val);
            }
            return Some(self.eval_unary(&UnaryOp::Neg, val, span));
        }
        if method == "not" && args.len() == 1 {
            // `not` covers both `!bool` (UnaryOp::Not) and `~int` (UnaryOp::BitNot).
            // Kāra disjointly types these, so the runtime value shape is unambiguous.
            let val = self.eval_expr_inner(&args[0].value);
            if self.pending_cf.is_some() {
                return Some(val);
            }
            let op = match &val {
                Value::Bool(_) => UnaryOp::Not,
                _ => UnaryOp::BitNot,
            };
            return Some(self.eval_unary(&op, val, span));
        }
        None
    }
}
