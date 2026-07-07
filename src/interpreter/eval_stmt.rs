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
use super::value::Value;
use super::Interpreter;

impl<'a> super::Interpreter<'a> {
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
            // After a successful let-binding, push a Drop slot for each
            // name the pattern introduced.
            push_drops_for_stmt(stmt, &mut cleanup);
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
                self.env.pop_scope();
                return Err(cf);
            }
            if self.observed_test_deadline_exceeded() {
                self.timed_out = true;
                let cf = ControlFlow::TimedOut;
                let path = ExitPath::classify(&cf);
                self.run_cleanup(&cleanup, &errdefers, &path);
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
            let v = self.eval_expr_inner(expr);
            if let Some(cf) = self.pending_cf.take() {
                let path = ExitPath::classify(&cf);
                self.signal_cancellation_if_error(&cf);
                self.run_cleanup(&cleanup, &errdefers, &path);
                self.env.pop_scope();
                return Err(cf);
            }
            v
        } else {
            Value::Unit
        };
        // Normal exit — drop+defer phase only.
        self.run_cleanup(&cleanup, &errdefers, &ExitPath::Normal);
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

        // 2. Merge defined variables
        self.env.push_scope();
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
                self.env.pop_scope();
                return Err(cf);
            }
        }

        // 4. Final expression (par blocks don't have a final_expr in current design)
        let result = if let Some(ref expr) = block.final_expr {
            let v = self.eval_expr_inner(expr);
            if let Some(cf) = self.pending_cf.take() {
                self.env.pop_scope();
                return Err(cf);
            }
            v
        } else {
            Value::Unit
        };
        self.env.pop_scope();
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
    /// `cleanup` front-to-back so program-order is preserved on
    /// in-place removal; the relative LIFO order of remaining
    /// entries is unchanged. Drop firings are recorded on
    /// `drop_trace` directly here (rather than via `run_cleanup`)
    /// so test traces include NLL and scope-exit firings in their
    /// actual program order.
    fn fire_due_drops(
        &mut self,
        cleanup: &mut Vec<CleanupAction>,
        last_use: &HashMap<String, usize>,
        stmt_idx: usize,
    ) {
        let mut i = 0;
        while i < cleanup.len() {
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
            } else {
                i += 1;
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
    fn invoke_user_drop_if_applicable(&mut self, name: &str) {
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
            return;
        }
        // A `#[compiler_builtin]` stdlib `impl Drop` (e.g.
        // `PooledConnection`) releases a side-table resource the
        // interpreter owns rather than running a Kāra body, so route it
        // to the native handler before the (placeholder) body drain.
        if self.try_eval_builtin_drop(&type_name, name) {
            return;
        }
        self.run_user_drop_body(&type_name, name);
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
        self.run_user_drop_body_on_value(type_name, value);
    }

    /// Value-based core of `run_user_drop_body` — also used by the
    /// fresh-temp call-arg drop hook in `eval_call` (B-2026-07-01-8's
    /// second half: `consume(Guard { id: 7 })` / `consume(Sig.A(1))` had
    /// no binding for the name-keyed runner to resolve).
    pub(crate) fn run_user_drop_body_on_value(&mut self, type_name: &str, value: Value) {
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
    fn suppress_let_rebind_user_drop(&mut self, stmt: &Stmt, cleanup: &mut Vec<CleanupAction>) {
        let source_name = match &stmt.kind {
            StmtKind::Let { value, .. } => match &value.kind {
                ExprKind::Identifier(n) => n.clone(),
                _ => return,
            },
            _ => return,
        };
        // Only suppress when the source's value has a user impl Drop.
        let type_name = match self.env.get(&source_name) {
            Some(Value::Struct { name, .. }) => name.clone(),
            // Enum-Drop parity — see `suppress_tail_expr_user_drop`.
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
                            return Err(cf);
                        }
                        v
                    }
                } else {
                    let v = self.eval_expr_inner(value);
                    if let Some(cf) = self.pending_cf.take() {
                        self.pending_tensor_fill = saved_tensor_fill;
                        return Err(cf);
                    }
                    v
                };
                self.pending_tensor_fill = saved_tensor_fill;
                // Capture for snapshot if this name is being watched.
                // We must clone before `bind_pattern` consumes `val`.
                if let crate::ast::PatternKind::Binding(name) = &pattern.kind {
                    if self.let_snapshot_watch.contains(name) {
                        self.captured_let_values.insert(name.clone(), val.clone());
                    }
                }
                self.bind_pattern(pattern, val);
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
                if self.try_match_pattern(pattern, &val) {
                    self.bind_pattern(pattern, val);
                } else {
                    self.eval_block_inner(else_block)?;
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
                let val = self.eval_expr_inner(value);
                if !self.assign_to_place(target, val) {
                    unreachable!(
                        "unsupported assignment target at {}:{}; should be caught by parser/typechecker",
                        stmt.span.line, stmt.span.column
                    );
                }
            }
            StmtKind::CompoundAssign { target, op, value } => {
                let current = self.eval_expr_inner(target);
                let rhs = self.eval_expr_inner(value);
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
                if let ExprKind::Call { callee, .. } = &expr.kind {
                    if let ExprKind::Identifier(fn_name) = &callee.kind {
                        if let Some(tn) = self.user_fn_return_type_name(fn_name) {
                            if self.program.drop_method_keys.contains_key(&tn) {
                                self.run_user_drop_body_on_value(&tn, discarded);
                            }
                        }
                    }
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
                let rhs = self.eval_expr_inner(&args[1].value);
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
            return Some(self.eval_unary(&UnaryOp::Neg, val, span));
        }
        if method == "not" && args.len() == 1 {
            // `not` covers both `!bool` (UnaryOp::Not) and `~int` (UnaryOp::BitNot).
            // Kāra disjointly types these, so the runtime value shape is unambiguous.
            let val = self.eval_expr_inner(&args[0].value);
            let op = match &val {
                Value::Bool(_) => UnaryOp::Not,
                _ => UnaryOp::BitNot,
            };
            return Some(self.eval_unary(&op, val, span));
        }
        None
    }
}
