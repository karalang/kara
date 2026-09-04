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
use super::{ConsoleSeg, ConsoleStream, Interpreter};

impl<'a> super::Interpreter<'a> {
    /// B-2026-08-14-6 — apply the implicit int-to-float widening to an
    /// assignment RHS the typechecker flagged.
    ///
    /// The statement sibling of `coerce_float_slot_arg` (container arguments).
    /// The typechecker records every integer expression that sits in a float
    /// slot; this converts at the assignment store, where the interpreter would
    /// otherwise write an `Int` into a slot the program declared `f64`.
    /// Non-flagged RHSs and already-`Float` values pass through, so it is inert
    /// everywhere else and idempotent where it fires.
    pub(super) fn coerce_float_assign_rhs(&self, value: &crate::ast::Expr, val: Value) -> Value {
        let Some(size) = self
            .typecheck_result
            .float_coerced_arg_sites
            .get(&crate::resolver::SpanKey::from_span(&value.span))
        else {
            return val;
        };
        match val {
            // B-2026-08-30-34 — convert the VALUE, not the carrier: a `u64` at
            // or above 2^63 rides as its negative two's-complement image, so
            // `n as f64` here answered -1 for `u64::MAX`.
            Value::Int(n) => super::round_float_to_declared_size(
                super::eval_expr::carrier_to_f64(n, self.span_unsigned_int_width(&value.span)),
                *size,
            ),
            other => other,
        }
    }

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
        // TAKEN, not read: `eval_body_growing` sets this immediately before a
        // function/closure/method body, and consuming it here means every
        // block nested inside that body sees `false`. See the field's doc.
        let is_fn_body = std::mem::take(&mut self.next_block_is_fn_body);
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
        // B-2026-08-28-22 — the same adoption for a conditionally-returned
        // owned param, seeded by the call. Gated on `is_fn_body` so it reaches
        // only the callee's own body block; a nested block inside that body
        // must NOT own the slot, or `record_conditional_move_tail` would treat
        // the binding as this block's and decline to disarm it.
        if is_fn_body {
            for name in std::mem::take(&mut self.pending_param_drop_bindings) {
                cleanup.push(CleanupAction::Drop { name });
            }
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
            // B-2026-08-30-33 — the interpreter twin of codegen's
            // `arm_conditional_store_flag`. A statement that hands an adopted
            // parameter to a new owner disarms its per-path body drop, so the
            // callee frame runs the body only on the path where the value
            // actually died.
            //
            // BEFORE the statement, not after: a `return` unwinds, so a hook
            // placed after it would never run on the very path that needs it.
            self.disarm_cond_store_param_on_handover(stmt);
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
            // B-2026-08-28-51 — seed the conditional-move escaping-site set for
            // the two escaping STATEMENT positions before the statement runs,
            // so a branch arm reached during it already knows its tail escapes.
            self.note_escaping_stmt_sites(stmt);
            // B-2026-08-28-51 — the `return` sibling of the block-tail hook. A
            // `return r;` nested inside a branch is the same conditional move:
            // the static retraction below targets THIS block's cleanup vector,
            // which does not hold a binding declared in an enclosing block, so
            // it silently does nothing and the enclosing drain fires the body a
            // second time. Reaching this statement is the runtime proof that
            // this path returned the value. A top-level `return r;` still owns
            // its binding here and takes the static path unchanged.
            if let StmtKind::Expr(e) = &stmt.kind {
                if let ExprKind::Return(Some(inner)) = &e.kind {
                    self.record_conditional_move_tail(inner, &cleanup);
                }
            }
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
            // B-2026-08-30-51 — snapshot any same-scope binding this statement
            // is about to shadow. It has to happen here: evaluating the `let`
            // overwrites the slot, and the old value is then unreachable.
            let shadowed_before = self.snapshot_shadowed_bindings(stmt);
            // B-2026-08-31-7 — the interpreter twin of codegen's
            // `clear_stale_param_view_marks`, and it has to land with it.
            // `owned_param_names_stack`'s top frame is this frame's view set,
            // and — exactly like `param_view_locals` on the other side — every
            // site inserted into it and none removed, so a name marked a view
            // by an earlier construct stayed one across a later, unrelated
            // `let` of the SAME NAME. View-ness means "the caller runs the
            // body", so the fresh value's body ran nowhere:
            //
            //   match t { E.A(r) => { let m = r; … } E.B => {} }
            //   let r = R { id: 98 };   // fresh, unrelated, same name
            //   let m2 = r;             // `src_is_view` reads the STALE mark
            //
            // printed `arm2 fresh98 dR2` where `dR98` is due as well. Both
            // backends had it and agreed, which is why it stayed invisible;
            // repairing only codegen would have converted an agreed-wrong
            // answer into a fresh divergence, the trade this file's match-arm
            // comment already warns against.
            self.clear_stale_param_view_marks(stmt);
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
            // B-2026-09-01-18 — the same rule for a struct literal DISCARDED as
            // a bare statement (`W { r: t, b: 1 };`, `{ W { r: t, b: 1 } };`),
            // which is not a `Let` and so never reached the hook above.
            self.suppress_discarded_literal_source_user_drops(stmt, &mut cleanup);
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
            // B-2026-08-29-24 — `let s = S { r: r };` wraps a param VIEW into a
            // fresh local; the caller runs that field's body, so mask it out of
            // this binding's field walk. Same channel as the move-out above.
            self.mask_param_view_struct_literal_fields(stmt);
            // B-2026-08-29-24 — the enum sibling: `let w = W2.Two(r, …)` moves
            // a param view into ONE payload slot; mask that slot alone.
            self.mask_param_view_enum_ctor_slots(stmt);
            // B-2026-08-29-24 — and the tuple sibling: `let t = (r, 5);`.
            self.mask_param_view_tuple_literal_elems(stmt);
            // B-2026-08-29-44 — a whole-value REBIND (`let s2 = s;`) inherits
            // the source binding's masks. Every mask above is keyed on the
            // BINDING, and a rebind gives the destination a fresh unmasked
            // walk, so the view's body ran again. The ALL-VIEWS case has no
            // such hole — it marks the binding a param view and view-ness
            // already propagates — so only the MIXED case loses its mask.
            // Codegen's twin is `transfer_move_masks_on_rebind`, called at the
            // same `let`; the two land together because masking on one backend
            // alone turns this agreed defect into a run-vs-build divergence.
            self.transfer_move_masks_on_rebind(stmt);
            // B-2026-08-29-45 — the ARRAY / `Vec`-literal sibling of the three
            // masks above. Codegen's twin is the
            // `container_literal_elems_are_all_param_views` gate at the same
            // `let` registration.
            self.mask_param_view_container_literal_elems(stmt);
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
            // B-2026-08-30-51 — freeze the value of any binding this `let`
            // SHADOWS, before the fresh slot below claims the same name.
            //
            // Ordering is the whole correctness argument, in both directions.
            // AFTER every move-suppression helper above: `let z = z;` moves the
            // old value into the new binding, and `suppress_let_rebind_user_drop`
            // has by now retracted the source's slot, so there is nothing left
            // to freeze and the one object keeps its single owner. BEFORE
            // `push_drops_for_stmt`: once that runs, a slot with this name is
            // ambiguous between the old binding and the new one.
            self.freeze_shadowed_drop_slots(stmt, &mut cleanup, &shadowed_before);
            // B-2026-09-02-17 — and a binding taken out of a container ELEMENT
            // the container still owns registers no slot either, for the same
            // caller-retains reason: `v[i]` is a `ref T`, so the container's
            // own walk runs the body.
            if !self.let_destructures_owned_param(stmt)
                && !Self::let_binds_borrowed_container_elem(stmt)
            {
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
        if is_fn_body {
            // B-2026-08-28-51 — the third escaping site: a function (or
            // closure / method) body's tail value goes to the caller.
            if let Some(ref expr) = block.final_expr {
                self.note_escaping_site(expr);
            }
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
            // B-2026-08-28-51 — must run BEFORE the static retraction below,
            // which removes from `cleanup` the very entry this consults to tell
            // a binding THIS block owns from one declared in an enclosing block.
            // B-2026-08-29-21 — a `return <ident>` in a block's TAIL position
            // (`if k { return r }`, no semicolon) is a `return` the statement
            // hook above never sees: it is the block's final expression, not a
            // statement, so neither `note_escaping_stmt_sites` nor the
            // statement-loop `record_conditional_move_tail` reaches it. Seed and
            // record it here, on the OPERAND, exactly as the statement spelling
            // does. Seeding is unconditional and safe because a `return`
            // operand always escapes — that is what distinguishes it from the
            // enclosing `if`, which is a discarded statement and deliberately
            // not a seed.
            //
            // Without this the interpreter ran the body TWICE on the returning
            // path once the predicate started admitting the mixed spelling:
            // `drop a` / `got 1` / `drop a`, against one body on all three
            // compiled backends, whose `guard_user_drop_for_nested_return` is
            // reached from the `return` expression itself and so never had the
            // gap.
            if let ExprKind::Return(Some(inner)) = &expr.kind {
                self.note_escaping_site(inner);
                self.record_conditional_move_tail(inner, &cleanup);
            }
            self.record_conditional_move_tail(expr, &cleanup);
            // B-2026-08-29-57 — the two move-records below must see THROUGH a
            // tail `return`, exactly as the statement loop's
            // `suppress_return_stmt_user_drop` and its container twin already
            // do for `return r;`. Both match on `Identifier`, so handed the
            // `Return` node itself they silently did nothing, and a function
            // whose body ends `return out` with NO trailing semicolon dropped
            // `out` at its own scope exit as well as handing it to the caller:
            // `mid dR1 v1 dR1` from the interpreter against `mid v1 dR1` from
            // every compiled backend. The same function written `return out;`
            // was correct, and so was the bare tail `out` -- the semicolon was
            // the whole difference, because it decides whether the `Return`
            // arrives here as a `final_expr` or up there as a statement.
            //
            // B-2026-08-29-21 added this same unwrap for `note_escaping_site`
            // and `record_conditional_move_tail` and stopped there; this is the
            // rest of that hook.
            let moved_out = match &expr.kind {
                ExprKind::Return(Some(inner)) => inner.as_ref(),
                _ => expr,
            };
            self.suppress_tail_expr_user_drop(moved_out, &mut cleanup);
            // Container twin: a bare-identifier tail moves the container out
            // as the block's result.
            self.record_container_bodies_move_sources(moved_out);
            // B-2026-08-30-34 — a block's TAIL is the other way a function
            // hands back a value (`fn f(v: u64) -> f64 { v }`), and the
            // caller-side widening cannot recover the source's signedness from
            // the value alone. Inert unless the typechecker recorded THIS span
            // as an integer landing in a float slot, so it is a no-op for every
            // block that is not one.
            let v = self.eval_expr_inner(expr);
            let v = self.coerce_float_assign_rhs(expr, v);
            if let Some(cf) = self.pending_cf.take() {
                let path = ExitPath::classify(&cf);
                self.signal_cancellation_if_error(&cf);
                self.run_cleanup(&cleanup, &errdefers, &path);
                self.capture_watched_bindings();
                self.env.pop_scope();
                return Err(cf);
            }
            // A function body whose TAIL is a syntactic `Err(...)` / `None`
            // leaves via the failure path even though no `ControlFlow` was
            // raised — nothing propagated, the value simply IS the error. The
            // block-exit code below classified that as `ExitPath::Normal`, so
            // `errdefer` never fired and the interpreter silently skipped
            // cleanup that both compiled backends performed (B-2026-08-23-9).
            //
            // The predicate is `ast::is_error_exit_value`, the same one
            // codegen's tail emitter uses, so the two agree by construction
            // rather than by convention. It is syntactic: `Err(e)` fires,
            // `if c { Err(e) } else { Ok(v) }` does not, on BOTH backends.
            //
            // The payload for an `errdefer(e)` binding comes from the ALREADY
            // EVALUATED tail value, so the argument expression is never run
            // twice — codegen has to re-compile or word-extract it precisely
            // because it has no value in hand at that point.
            if is_fn_body && crate::ast::is_error_exit_value(expr) {
                let path = ExitPath::classify_tail_value(&v);
                if path.is_error() {
                    self.run_cleanup(&cleanup, &errdefers, &path);
                    self.capture_watched_bindings();
                    self.env.pop_scope();
                    return Ok(v);
                }
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
        // Each branch result: (index, defined_vars, console_segs, dbg_lines,
        // runtime_errors, control_flow_or_value)
        type BranchResult = (
            usize,
            HashMap<String, Value>,
            Vec<ConsoleSeg>,
            Vec<String>,
            Vec<crate::interpreter::RuntimeError>,
            (Vec<crate::interpreter::ErrorTraceFrame>, bool),
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
                    // ONE capture for both console streams, so the join can
                    // replay this branch's stdout AND stderr in source order
                    // with their within-branch interleaving intact
                    // (B-2026-08-23-15). Not `captured_output`, which is
                    // stdout-only.
                    branch_interp.captured_console = Some(Vec::new());
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

                    // B-2026-08-07-22 — separate the SEEDED environment from
                    // what this branch actually introduces, by giving the
                    // branch's own bindings a scope of their own.
                    //
                    // The join below merges `scopes.last()` back into the
                    // enclosing scope. Without this push, that map is the
                    // whole flattened `env.snapshot()` seeded just above —
                    // `snapshot()` collapses every scope into one map and
                    // `define()` writes them all into the branch's single
                    // scope — so the join re-`define`d EVERY enclosing
                    // variable into the parent's CURRENT scope. At function
                    // top level that is harmless (it rewrites the bindings in
                    // their own scope), which is why every existing par test
                    // passed. One block deeper it is not: inside a `while` or
                    // `if` body the current scope is the block's, so each
                    // outer variable gained a SHADOW there, and the next
                    // `i = i + 1` updated the shadow and lost it when the
                    // block scope popped. A loop counter that never advances
                    // is an infinite loop; everywhere else it is a silently
                    // wrong answer.
                    branch_interp.env.push_scope();

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

                    let console = branch_interp.captured_console.unwrap_or_default();
                    let dbg_lines = branch_interp.captured_dbg.unwrap_or_default();
                    // B-2026-08-24-4 — carry the DIAGNOSTIC PAYLOAD across the
                    // join. `record_runtime_error` pushes the message, span and
                    // trace onto the BRANCH interpreter's vec; only the bare
                    // `ControlFlow::RuntimeError` marker used to reach the
                    // parent, so the CLI found an empty `runtime_errors`, had
                    // nothing to render, and exited 0 — a program that died
                    // halfway reported SUCCESS.
                    let branch_errors = std::mem::take(&mut branch_interp.runtime_errors);
                    // The RETURN TRACE is a second vec on the branch
                    // interpreter and dies with it the same way. Harvest it
                    // only when the branch actually recorded an error: a
                    // successful branch can still have pushed frames (a `?`
                    // propagating an `Err` is ordinary control flow, not a
                    // fault), and those frames would otherwise be rendered
                    // under a SIBLING's error as if they were part of its
                    // trace.
                    let branch_trace = if branch_errors.is_empty() {
                        (Vec::new(), false)
                    } else {
                        (
                            std::mem::take(&mut branch_interp.error_trace),
                            branch_interp.error_trace_truncated,
                        )
                    };

                    results_ref.lock().unwrap().push((
                        i,
                        defined_vars,
                        console,
                        dbg_lines,
                        branch_errors,
                        branch_trace,
                        cf_result,
                    ));
                });
            }
        });

        // Sort results by source order (deterministic)
        let mut branch_results = results.into_inner().unwrap();
        branch_results.sort_by_key(|(i, _, _, _, _, _, _)| *i);

        // Merge results back into the parent interpreter
        // 1. Replay console output in source order — this is the join half of
        //    design.md § dbg()'s console-chokepoint promise ("a parallel
        //    branch's writes are captured and replayed at the join in source
        //    order"), and it covers stderr as well as stdout: before
        //    B-2026-08-23-15 stderr bypassed the capture entirely and landed
        //    in thread-completion order, so `eprintln` under `par {}` was
        //    nondeterministic in the interpreter while the compiled backends
        //    replayed it in source order.
        //
        //    Replaying THROUGH `write_stdout` / `write_stderr` rather than to
        //    the fd is deliberate, and mirrors the runtime's `OUTPUT_REDIRECT`
        //    note: in a NESTED `par {}` the parent is itself a branch, so its
        //    own `captured_console` picks these segments up and defers them to
        //    the outer join. It also keeps stdout on the one writer that
        //    handles a closed reader (B-2026-08-19-2) instead of the bare
        //    `print!` this loop used to call, which panics on `BrokenPipe`.
        let replay: Vec<ConsoleSeg> = branch_results
            .iter()
            .flat_map(|(_, _, console, _, _, _, _)| console.iter().cloned())
            .collect();
        for seg in replay {
            match seg.stream {
                ConsoleStream::Stdout => self.write_stdout(&seg.text, false),
                ConsoleStream::Stderr => self.write_stderr(&seg.text, false),
            }
        }

        // 1b. Merge dbg lines in source order (test-only; only present
        // when the parent has an active capture buffer).
        if let Some(ref mut cap) = self.captured_dbg {
            for (_, _, _, dbg_lines, _, _, _) in &branch_results {
                for line in dbg_lines {
                    cap.push(line.clone());
                }
            }
        }

        // 1c. Merge each branch's runtime errors in source order
        //     (B-2026-08-24-4). Same ordering discipline as the console replay
        //     above, and for the same reason: the join is where a branch's
        //     observable output becomes the parent's, and a diagnostic is
        //     output. Every branch contributes — the CLI renders the whole vec,
        //     so a par block that killed two branches reports both rather than
        //     silently picking one. Done BEFORE the control-flow check below,
        //     which returns early on the first error and would otherwise skip
        //     the merge entirely.
        for (_, _, _, _, errors, (trace, truncated), _) in &branch_results {
            self.runtime_errors.extend(errors.iter().cloned());
            self.error_trace.extend(trace.iter().cloned());
            self.error_trace_truncated |= *truncated;
        }

        // 2. Merge defined variables into the CURRENT (enclosing) scope so
        //    they outlive the `par {}` block — the join barrier hoists each
        //    branch's `let` into the enclosing scope, matching the resolver /
        //    typechecker and the shape `par { let a = f(); let b = g(); }
        //    (a, b)` needs (B-2026-07-11-3). No private scope is pushed.
        for (_, vars, _, _, _, _, _) in &branch_results {
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
        for (_, _, _, _, _, _, result) in branch_results {
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
            // B-2026-08-30-34 — a block's TAIL is the other way a function
            // hands back a value (`fn f(v: u64) -> f64 { v }`), and the
            // caller-side widening cannot recover the source's signedness from
            // the value alone. Inert unless the typechecker recorded THIS span
            // as an integer landing in a float slot, so it is a no-op for every
            // block that is not one.
            let v = self.eval_expr_inner(expr);
            let v = self.coerce_float_assign_rhs(expr, v);
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
                // B-2026-08-30-51 — a shadowed binding drops its OWN frozen
                // value rather than resolving its name, which by now addresses
                // the survivor. Value-based because the name is the thing that
                // stopped identifying it; `run_discarded_value_user_drops` is
                // the same walk `let _ = …` uses, and a shadowed value is
                // discarded in exactly that sense. The trace keeps the SOURCE
                // name, so existing drop-order assertions read unchanged.
                CleanupAction::DropShadowed { name, value } => {
                    self.run_discarded_value_user_drops(value.clone());
                    self.drop_trace.push(name.clone());
                }
            }
        }
    }

    /// B-2026-08-30-51 — the values of the same-scope bindings a `let` is about
    /// to shadow, captured before it runs.
    ///
    /// INNERMOST SCOPE ONLY. Shadowing an OUTER binding is a different event:
    /// that binding keeps its own slot in its own block's cleanup and is live
    /// again once this block ends, so freezing it here would fire its body
    /// early and then again at its real owner.
    ///
    /// A name the pattern binds more than once cannot happen (the resolver
    /// rejects it), so one entry per name is enough.
    fn snapshot_shadowed_bindings(&self, stmt: &Stmt) -> Vec<(String, Value)> {
        let pattern = match &stmt.kind {
            StmtKind::Let { pattern, .. } | StmtKind::LetElse { pattern, .. } => pattern,
            _ => return Vec::new(),
        };
        pattern
            .binding_names()
            .into_iter()
            .filter_map(|n| self.env.get_in_current_scope(&n).map(|v| (n, v)))
            .collect()
    }

    /// B-2026-08-30-51 — convert each shadowed binding's live `Drop` slot into a
    /// [`CleanupAction::DropShadowed`] carrying the value it actually owns.
    ///
    /// Only a slot that is STILL PRESENT is converted, and that is what keeps
    /// `let z = z;` at one body: the move-suppression helpers run first and have
    /// already retracted the source's slot for a rebind, so there is nothing
    /// here to freeze and the single object keeps its single owner. The same
    /// holds for every other retraction upstream — `forget(x)`, an assign move,
    /// a moved-out field — each of which means "this binding no longer owns a
    /// body", and none of which should be resurrected by this pass.
    ///
    /// The LAST matching slot is converted, not the first. Slots for one name
    /// accumulate in program order under repeated shadowing (`let w` three
    /// times), so the newest is the one this `let` displaces; taking the first
    /// would re-freeze an already-frozen generation's slot.
    fn freeze_shadowed_drop_slots(
        &mut self,
        stmt: &Stmt,
        cleanup: &mut [CleanupAction],
        shadowed: &[(String, Value)],
    ) {
        if shadowed.is_empty() {
            return;
        }
        if !matches!(&stmt.kind, StmtKind::Let { .. } | StmtKind::LetElse { .. }) {
            return;
        }
        for (name, value) in shadowed {
            let Some(idx) = cleanup
                .iter()
                .rposition(|a| matches!(a, CleanupAction::Drop { name: n } if n == name))
            else {
                continue;
            };
            cleanup[idx] = CleanupAction::DropShadowed {
                name: name.clone(),
                value: value.clone(),
            };
        }
    }

    /// Fire any `Drop` slot whose binding's last use was the just-    /// Fire any `Drop` slot whose binding's last use was the just-
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
                // B-2026-08-30-51 — a shadowed slot fires at the NAME's endpoint,
                // the same one the survivor uses, which is what the compiled
                // backends do: their `last_use` is name-keyed too, so every
                // generation of a shadowed name shares one endpoint and they
                // drain there LIFO -- `dR2 dR1`, newest first. Holding the
                // shadowed value to scope exit instead ran the right bodies in
                // the wrong place, measured as all of them landing after the
                // last statement of `main` rather than beside the survivor's.
                CleanupAction::DropShadowed { name, .. } => {
                    last_use.get(name).copied() == Some(stmt_idx)
                }
                CleanupAction::Defer(_) => false,
            };
            if should_fire {
                let action = cleanup.remove(i);
                match action {
                    CleanupAction::Drop { name } => {
                        // Phase 7 user-`impl Drop` dispatch Prereq.4 — fire
                        // the user body at NLL endpoint before pushing the
                        // trace record, mirroring the scope-exit drain
                        // arm in `run_cleanup`.
                        self.invoke_user_drop_if_applicable(&name);
                        self.drop_trace.push(name);
                    }
                    // B-2026-08-30-51 — value-based, for the reason the variant
                    // exists: this slot's name now addresses the survivor.
                    CleanupAction::DropShadowed { name, value } => {
                        self.run_discarded_value_user_drops(value);
                        self.drop_trace.push(name);
                    }
                    CleanupAction::Defer(_) => {}
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
        // B-2026-08-03-3 — per-ELEMENT move-outs (`let x = t.0`). Only the
        // moved indices are skipped; the tuple's other elements still die here.
        // Empty for every array binding (only a tuple index can be moved out
        // this way), so the common path is unchanged.
        let moved: Vec<usize> = self
            .moved_out_tuple_elem_bodies
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, i)| *i)
            .collect();
        let payload_masked: Vec<usize> = self
            .moved_out_tuple_elem_payload_bodies
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, i)| *i)
            .collect();
        for (ei, e) in elems.into_iter().enumerate() {
            if moved.contains(&ei) {
                continue;
            }
            // B-2026-08-01-23 — a nested container element (`Vec[Vec[Res]]`):
            // recurse to the innermost struct elements.
            if let Value::Array(_) = &e {
                self.run_nested_array_struct_elem_bodies(&e);
                continue;
            }
            // B-2026-08-02-22 — a TUPLE element (`Vec[(Res, i64)]`): run each
            // tuple item's own body (forward order, matching codegen's
            // struct-GEP order in `__karac_dropelems_tuple_*`) and then its
            // field bodies. Without this the element was silent while
            // codegen's new vec-of-tuple walker fired — a run-vs-build
            // divergence in the silent direction on this side.
            if let Value::Tuple(items) = &e {
                let items = items.clone();
                self.run_tuple_item_user_drops(items);
                continue;
            }
            // B-2026-08-03-1 — an `Option[P]` / `Result[O, E]` ELEMENT
            // (`Vec[Option[Res]]`): run the live payload's bodies through the
            // value-driven recursion the discard path uses for these
            // built-ins. Codegen twin: the Option/Result arm of
            // `emit_nested_vec_elem_bodies_fn`.
            if let Value::EnumVariant { enum_name, .. } = &e {
                if enum_name == "Option" || enum_name == "Result" {
                    self.run_discarded_value_user_drops(e);
                    continue;
                }
            }
            // B-2026-08-28-47 — a user ENUM element (`let p = (E.B, 1)`).
            // None of the arms above match it and the struct arm below does
            // not either, so it fell to `value_runs_user_drop`, whose
            // `EnumVariant` arm admits only `Option`/`Result` — the element
            // ran NO body on any backend, while the same tuple destructured
            // one statement later ran it on all three. Own body first, then
            // the live variant's payload bodies, matching the Vec-FIELD arm
            // in `drop_user_drop_fields_of_value` and codegen's
            // `emit_slot_drop_bodies_at` enum leg.
            // B-2026-08-28-55 — this loop serves BOTH tuple and Vec bindings,
            // and the arm was gated to tuples alone while
            // `emit_vec_elem_user_drop_bodies_fn`'s SELECTOR was enum-blind.
            // That selector was widened in the same commit as this line, so
            // both containers now run the body and the gate would only
            // reintroduce the divergence it was added to prevent.
            if let Value::EnumVariant { enum_name, .. } = &e {
                // Own-`Drop` enums ONLY, matching codegen's selection rule
                // exactly: its walkers reach an enum member through
                // `type_runs_user_drop`, which answers true for an enum only
                // via `drop_method_keys`. An enum with NO own `Drop` but a
                // Drop-bearing payload (`enum E2 { A(R), B }`) is therefore
                // invisible to codegen in these positions, so running the
                // payload walk here would turn a both-backends-silent shape
                // into a run-vs-build divergence. Measured: it does exactly
                // that. That shape is real and filed separately.
                // B-2026-08-28-54 — the payload walk is UNCONDITIONAL while the
                // own body stays gated. An enum with no `impl Drop` of its own
                // but a Drop-bearing variant payload is Drop-relevant content,
                // and codegen's `type_runs_user_drop` now sees it through
                // `enum_variant_field_type_exprs`. Gating both on
                // `drop_method_keys` (which 9727cdb did, deliberately, while
                // codegen was still blind) left `E2.A(R)` silent here.
                let tn = enum_name.clone();
                if self.program.drop_method_keys.contains_key(&tn) {
                    self.run_user_drop_body_only(&tn, e.clone());
                }
                // B-2026-08-29-33 — a consuming arm over `<name>.<ei>` took
                // this element's PAYLOAD, so its body belongs to the arm now.
                // The element's own body above is NOT masked with it: the enum
                // object did not move. Masking through `moved_out_tuple_elem_
                // bodies` (the `moved` list above) would skip the element
                // wholesale and lose that own body.
                if !payload_masked.contains(&ei) {
                    self.run_enum_payload_user_drops_value(&e);
                }
                continue;
            }
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
        // Map-values leg: same treatment for a `Map` binding's stored values,
        // plus the KEY half (B-2026-08-26-41).
        //
        // KEY FIRST, then value. An entry's two halves drop in the order they
        // are declared — the rule a struct's fields already follow — and
        // `Map[K, V]` declares the key first. It also happens to be what the
        // compiled backend produces for free: its NLL drain fires a frame's
        // actions in reverse-introduction order, and the value walk is
        // registered before the key walk, so the key fires first there. The
        // bodies are observable, so the two backends have to agree on this;
        // design.md fixes no order, but it has to be fixed somewhere.
        //
        // Both are consulted before the early return — a map whose KEY drops
        // and whose value does not must still short-circuit here rather than
        // fall through to `drop_target`.
        let ran_keys = self.run_map_key_user_drops(name);
        let ran_vals = self.run_map_val_user_drops(name);
        if ran_vals || ran_keys {
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
        // B-2026-08-03-8 — fields moved out by a `let x = h.f` belong to the
        // destination now. Dropping them from the value before the walk is the
        // whole mask: the walk resolves each declared field through
        // `fields.get(..)` and skips a missing one, so no gate has to be
        // threaded through its several other callers. Codegen's twin re-emits
        // the walker with the same field index masked.
        let moved: Vec<String> = self
            .moved_out_struct_field_bodies
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, f)| f.clone())
            .collect();
        // B-2026-09-03-11 — the same mask for a field moved out through a
        // CHAIN (`let (r, k) = g.h.pe;`). The flat set above is keyed
        // `(name, field)` and cannot express it, so the root's walk descended
        // into `h` and ran `pe`'s element body a second time.
        let nested: Vec<Vec<String>> = self
            .moved_out_nested_field_bodies
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, p)| p.clone())
            .collect();
        if !moved.is_empty() || !nested.is_empty() {
            if let Value::Struct {
                name: sname,
                mut fields,
            } = value
            {
                for f in &moved {
                    fields.remove(f);
                }
                let mut masked = Value::Struct {
                    name: sname,
                    fields,
                };
                for path in &nested {
                    Self::remove_field_at_path(&mut masked, path);
                }
                self.drop_user_drop_fields_of_value(&masked);
                return;
            }
        }
        self.seed_payload_masked_fields(name);
        self.drop_user_drop_fields_of_value(&value);
    }

    /// B-2026-08-29-33 — arm the one-shot top-level payload mask for `name`
    /// before a `drop_user_drop_fields_of_value` call, from the pairs a
    /// consuming arm over `<name>.<field>` recorded. No-op when nothing was
    /// taken, which is every walk that is not downstream of such an arm.
    pub(super) fn seed_payload_masked_fields(&mut self, name: &str) {
        let fields: std::collections::HashSet<Vec<String>> = self
            .moved_out_struct_field_payload_bodies
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, f)| f.clone())
            .collect();
        if !fields.is_empty() {
            self.pending_payload_masked_fields = Some(fields);
        }
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
        // B-2026-08-29-24 — blank out the payload slots whose body belongs to
        // somebody else (a constructor moved a param VIEW in). Masking the
        // VALUE rather than threading a gate through the walk is the struct
        // mask's idiom one container over: the walk skips a slot that is not a
        // `Value::Struct`, and blanking preserves index alignment with the
        // declared payload list, which removing an item would not.
        let masked: Vec<usize> = self
            .moved_out_enum_payload_slots
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, i)| *i)
            .collect();
        if !masked.is_empty() {
            if let Value::EnumVariant {
                enum_name,
                variant,
                data: EnumData::Tuple(items),
            } = value
            {
                let mut items = items.clone();
                for i in masked {
                    if let Some(slot) = items.get_mut(i) {
                        *slot = Value::Unit;
                    }
                }
                let masked_value = Value::EnumVariant {
                    enum_name: enum_name.clone(),
                    variant: variant.clone(),
                    data: EnumData::Tuple(items),
                };
                self.run_enum_payload_user_drops_value(&masked_value);
                return;
            }
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
        // B-2026-08-01-31 — accept a DEEP chain source (`let x = o.h.r`):
        // flatten to the root identifier and walk the value through the
        // middle fields. The record stays ROOT-coarse, matching codegen's
        // root-slot disarm. Depth 1 keeps the original behavior.
        let mut middles: Vec<&str> = Vec::new();
        let mut chain_cur = object;
        let src = loop {
            match &chain_cur.kind {
                ExprKind::Identifier(s) => break s,
                ExprKind::FieldAccess {
                    object: inner,
                    field: mid,
                } => {
                    middles.push(mid.as_str());
                    chain_cur = inner;
                }
                _ => return,
            }
        };
        middles.reverse();
        let mut parent = self.env.get(src);
        for mid in &middles {
            parent = match parent {
                Some(Value::Struct { fields, .. }) => fields.get(*mid).cloned(),
                _ => None,
            };
        }
        let Some(Value::Struct { fields, .. }) = parent else {
            return;
        };
        let Some(field_value) = fields.get(field).cloned() else {
            return;
        };
        if self.value_runs_user_drop(&field_value) {
            // B-2026-09-01-2 — DEPTH 1 registers no coarse record, because the
            // `let` arm has already registered the PRECISE `(src, field)` pair
            // for exactly this shape (B-2026-08-03-8, keyed on the same bare
            // `Identifier` object this branch requires). Two records for one
            // statement, and `drop_user_drop_fields_of_binding` tests the
            // coarse set FIRST and returns -- so the precise mask was dead code
            // for every depth-1 move and the source's OTHER fields lost their
            // bodies outright.
            //
            // `let s = S3 { a: r, b: mk(2) }; let x = s.a;` ran `dR1` against
            // every compiled backend's `dR2 dR1` -- `b` has no other owner, so
            // its body was simply lost. Both fields fresh
            // (`S3 { a: mk(5), b: mk(6) }`) loses it the same way, which is what
            // puts the axis on the shadowing rather than on the param view the
            // row was found through.
            //
            // The coarse record is kept for a DEEP chain (`let x = o.h.r`),
            // where the `let` arm registers nothing -- its precise insert
            // requires a bare identifier object -- so the root-coarse disarm is
            // still the only mask that shape has. That matches codegen's
            // root-slot disarm for the deep case exactly as before.
            //
            // The enum branch of `drop_user_drop_fields_of_binding` sits BEHIND
            // this early return, but no enum-valued source can reach it: both
            // inserts into this set are gated on a `Value::Struct` source (here,
            // and the passthrough disarm's `Some(v @ Value::Struct { .. })`
            // arm), and a rebind clears the record. Measured on an enum-valued
            // source either way.
            if !middles.is_empty() {
                self.moved_out_drop_field_bindings.insert(src.clone());
            }
        }
    }

    /// B-2026-08-29-24 — interpreter twin of codegen's
    /// `mask_param_view_struct_literal_fields`. `let s = S { r: r };` stores a
    /// param VIEW into a fresh local, and under caller-retains the caller runs
    /// that value's `Drop` body, so this binding's field walk ran it a second
    /// time. B-2026-08-29-19 covered the variant-constructor spelling of the
    /// same move; the struct literal is the same move through a different
    /// constructor.
    ///
    /// Routed through `moved_out_struct_field_bodies` — the per-field mask
    /// B-2026-08-03-8 built for a field moved OUT of a binding — because the
    /// destination is the same: a body this binding must stop running because
    /// something else runs it. That keeps the mask PER FIELD, so a mixed
    /// literal (`S3 { a: r, b: R { id: 2 } }`) loses only the view's body,
    /// matching codegen's masked walker slot for slot.
    ///
    /// The per-field admission test is `value_runs_user_drop` over the BOUND
    /// field value, because that is exactly what
    /// `drop_user_drop_fields_of_binding` will visit — each backend asking the
    /// question its OWN walker asks, the pairing B-2026-08-29-19 established.
    /// B-2026-08-29-44 — carry a binding's move-out masks across a WHOLE-VALUE
    /// REBIND. Copies rather than moves: the source is still live for its own
    /// walk and must stay masked too. Codegen's twin is the identically-named
    /// helper in `control_flow_match.rs`.
    fn transfer_move_masks_on_rebind(&mut self, stmt: &Stmt) {
        let StmtKind::Let { pattern, value, .. } = &stmt.kind else {
            return;
        };
        let PatternKind::Binding(dst) = &pattern.kind else {
            return;
        };
        let ExprKind::Identifier(src) = &value.kind else {
            return;
        };
        if src == dst {
            return;
        }
        let (src, dst) = (src.clone(), dst.clone());
        let fields: Vec<String> = self
            .moved_out_struct_field_bodies
            .iter()
            .filter(|(n, _)| n == &src)
            .map(|(_, f)| f.clone())
            .collect();
        for f in fields {
            self.moved_out_struct_field_bodies.insert((dst.clone(), f));
        }
        let elems: Vec<usize> = self
            .moved_out_tuple_elem_bodies
            .iter()
            .filter(|(n, _)| n == &src)
            .map(|(_, i)| *i)
            .collect();
        for i in elems {
            self.moved_out_tuple_elem_bodies.insert((dst.clone(), i));
        }
        // B-2026-09-03-8 — the two param-view RECORDS travel with the masks
        // above, one hop later and for the same reason. A mask says the walk
        // skips a slot; the record says WHY — the value is the caller's — and
        // only the record can answer whether a later `let x = t2.0;` may
        // register a body of its own. Transferring the masks alone left the
        // rebind correctly masked and then let the move-out mint a second
        // owner. Codegen's identically-named twin carries the same two.
        let vfields: Vec<String> = self
            .param_view_struct_fields
            .iter()
            .filter(|(n, _)| n == &src)
            .map(|(_, f)| f.clone())
            .collect();
        for f in vfields {
            self.param_view_struct_fields.insert((dst.clone(), f));
        }
        let velems: Vec<usize> = self
            .param_view_tuple_elems
            .iter()
            .filter(|(n, _)| n == &src)
            .map(|(_, i)| *i)
            .collect();
        for i in velems {
            self.param_view_tuple_elems.insert((dst.clone(), i));
        }
        // The ENUM-CTOR slot mask is DELIBERATELY NOT transferred, and this is
        // the one line of this fix that had to be measured rather than
        // reasoned. Copying it here works — the interpreter prints the due
        // `dR2 dR1` for `let w = W2.Two(r, mk(2)); let w2 = w;`. But codegen
        // CANNOT follow: it derives a constructor's view slots from the ctor
        // EXPRESSION at the `let` and stores nothing per variable, so a rebind
        // there has no mask to inherit and keeps its double. Transferring only
        // here therefore turns an AGREED defect into a run-vs-build divergence
        // — measured, `dR2 dR1` against `dR1 dR2 dR1` — which is the worse of
        // the two and the trade this row's family keeps refusing. The enum
        // spelling needs a per-var store on the codegen side first; it is
        // filed separately and stays agreed-but-wrong until then.
    }

    fn mask_param_view_struct_literal_fields(&mut self, stmt: &Stmt) {
        let StmtKind::Let { pattern, value, .. } = &stmt.kind else {
            return;
        };
        let PatternKind::Binding(bname) = &pattern.kind else {
            return;
        };
        let ExprKind::StructLiteral { fields, spread, .. } = &value.kind else {
            return;
        };
        // A spread fills fields from a base value this walk has not examined;
        // masking a subset against an unexamined base is how a body goes
        // missing. Codegen's twin bails on the same shape.
        if spread.is_some() {
            return;
        }
        // B-2026-08-27-48 — a method frame's arguments reach no caller-side
        // fire, so there is no other owner to hand the body to. Same guard
        // `let_destructures_owned_param` takes, for the same reason.
        if self.owned_param_frame_is_method.last().copied() == Some(true) {
            return;
        }
        let Some(Value::Struct {
            name: sname,
            fields: bound,
        }) = self.env.get(bname)
        else {
            return;
        };
        let mut views: Vec<String> = Vec::new();
        let mut visited = 0usize;
        for init in fields {
            let Some(fv) = bound.get(&init.name) else {
                return;
            };
            if !self.value_runs_user_drop(fv) {
                continue;
            }
            visited += 1;
            let is_view = matches!(&init.value.kind, ExprKind::Identifier(src)
                if self.owned_param_names_stack
                    .last()
                    .is_some_and(|params| params.contains(src.as_str())));
            if is_view {
                views.push(init.name.clone());
            }
        }
        if views.is_empty() {
            return;
        }
        let all_views = views.len() == visited;
        // B-2026-08-29-43 — the MIXED-literal bail that used to sit here is
        // gone, and it is gone on BOTH backends in one commit. It existed
        // because codegen's only surgery on an own-`Drop` struct was the
        // all-or-nothing `karac_dropnf_<T>` swap, so masking per field here
        // alone would have turned an agreed defect into a run-vs-build
        // divergence. `emit_user_drop_wrapper_skipping` gives codegen the
        // per-field variant, so both backends now mask the view's body and
        // keep the fresh field's — which is what the mask below already did
        // for a struct with no `impl Drop` of its own.
        let bname = bname.clone();
        for f in views {
            // B-2026-08-29-47 — the SAME pair into two sets. The mask says the
            // walk skips this field; the record says the value belongs to the
            // caller, which is what a later `let x = s.a;` has to ask before
            // registering a slot of its own. The mask alone cannot answer it —
            // a field moved out by an ordinary `let` lands there too, and that
            // one the destination genuinely owns.
            self.param_view_struct_fields
                .insert((bname.clone(), f.clone()));
            self.moved_out_struct_field_bodies
                .insert((bname.clone(), f));
        }
        // Every visited field was a view, so the binding hands out nothing of
        // its own: propagate view-ness the way B-2026-08-01-15's rebind does,
        // or `let s2 = s;` re-arms a full walk over the fields just masked.
        // Gated on the struct having no `impl Drop` of its own — that body IS
        // the binding's own and no caller runs it, so a view mark there would
        // silence it. Codegen's twin gets the gate for free: its arm is
        // `has_field_user_drop`, which is `!has_user_drop`.
        if all_views && !self.program.drop_method_keys.contains_key(&sname) {
            if let Some(top) = self.owned_param_names_stack.last_mut() {
                top.insert(bname);
            }
        }
    }

    /// B-2026-08-29-24 — the enum sibling of
    /// [`Self::mask_param_view_struct_literal_fields`], and the half
    /// B-2026-08-29-19 had to leave wrong. That fix withheld the payload walk
    /// WHOLE when every slot the constructor filled was a param view, and could
    /// not act at all on a MIXED wrap (`W2.Two(r, R { id: 2 })`): one walk
    /// covered both slots, so suppressing it would have traded the view's
    /// doubled body for the fresh payload's missing one. Recording the view
    /// SLOT lets the walk skip it and keep the rest.
    ///
    /// Runs only where the all-views predicate declined, since that path
    /// suppresses the binding's Drop registration outright and there is then no
    /// walk left to mask.
    fn mask_param_view_enum_ctor_slots(&mut self, stmt: &Stmt) {
        let StmtKind::Let { pattern, value, .. } = &stmt.kind else {
            return;
        };
        let PatternKind::Binding(bname) = &pattern.kind else {
            return;
        };
        let ExprKind::Call { callee, args } = &value.kind else {
            return;
        };
        // B-2026-08-27-48 — a method frame's arguments reach no caller-side
        // fire; same guard the sibling and `let_destructures_owned_param` take.
        if self.owned_param_frame_is_method.last().copied() == Some(true) {
            return;
        }
        let variant = match &callee.kind {
            ExprKind::Identifier(n) => n.clone(),
            ExprKind::Path { segments, .. } => match segments.last() {
                Some(v) => v.clone(),
                None => return,
            },
            _ => return,
        };
        let Some(Value::EnumVariant {
            enum_name,
            variant: bound_variant,
            data: EnumData::Tuple(payloads),
        }) = self.env.get(bname)
        else {
            return;
        };
        // `Option` / `Result` payloads ride the `optres_*` machinery, which has
        // no walker to mask — the same exclusion, for the same reason, that
        // `let_ctor_payloads_are_param_views` carries.
        if enum_name == "Option" || enum_name == "Result" {
            return;
        }
        if bound_variant != variant {
            return;
        }
        let mut views: Vec<usize> = Vec::new();
        for (i, payload) in payloads.iter().enumerate() {
            if !self.value_runs_user_drop(payload) {
                continue;
            }
            let Some(arg) = args.get(i) else {
                return;
            };
            let is_view = matches!(&arg.value.kind, ExprKind::Identifier(src)
                if self.owned_param_names_stack
                    .last()
                    .is_some_and(|params| params.contains(src.as_str())));
            if is_view {
                views.push(i);
            }
        }
        let bname = bname.clone();
        for i in views {
            self.moved_out_enum_payload_slots.insert((bname.clone(), i));
        }
    }

    /// B-2026-08-29-24 — the tuple sibling of
    /// [`Self::mask_param_view_struct_literal_fields`]. `let t = (r, 5);` moves
    /// a param VIEW into an element; the caller runs that value's body, so this
    /// binding's element walk must skip it.
    ///
    /// Routed through `moved_out_tuple_elem_bodies`, the per-element mask
    /// B-2026-08-03-3 built for an element moved OUT of a tuple — the same
    /// destination for the same reason, and per element, so a mixed literal
    /// keeps the fresh element's body. Codegen's twin masks the same index out
    /// of `emit_tuple_elem_user_drop_bodies_fn_skipping`.
    /// B-2026-08-29-45 — an ARRAY / `Vec` literal EVERY element of which is a
    /// param VIEW: the bodies belong to the CALLER, so this binding must not
    /// walk its elements at all. `fn take(r: R) { let v: Vec[R] = [r]; }` ran
    /// `dR1` twice — once here, once at the caller's fire.
    ///
    /// ALL-OR-NOTHING, matching codegen's
    /// `container_literal_elems_are_all_param_views` exactly: the tuple sibling
    /// above can mask individual indices because a tuple's arity is fixed,
    /// while a `Vec`'s is not, so a MIXED literal is left as it was rather than
    /// half-fixed. The two backends have to draw this line in the same place or
    /// the mixed case becomes a run-vs-build divergence instead of an agreed
    /// gap.
    fn mask_param_view_container_literal_elems(&mut self, stmt: &Stmt) {
        let StmtKind::Let { pattern, value, .. } = &stmt.kind else {
            return;
        };
        let PatternKind::Binding(bname) = &pattern.kind else {
            return;
        };
        let elems: &[Expr] = match &value.kind {
            ExprKind::ArrayLiteral(elems) => elems,
            ExprKind::PrefixCollectionLiteral { items, .. } => items,
            _ => return,
        };
        // Same method-frame guard as the siblings (B-2026-08-27-48).
        if self.owned_param_frame_is_method.last().copied() == Some(true) {
            return;
        }
        if elems.is_empty() {
            return;
        }
        let all_views = elems.iter().all(|e| {
            matches!(&e.kind, ExprKind::Identifier(src)
                if self.owned_param_names_stack
                    .last()
                    .is_some_and(|params| params.contains(src.as_str())))
        });
        if !all_views {
            return;
        }
        self.moved_out_container_bodies_bindings
            .insert(bname.clone());
    }

    fn mask_param_view_tuple_literal_elems(&mut self, stmt: &Stmt) {
        let StmtKind::Let { pattern, value, .. } = &stmt.kind else {
            return;
        };
        let PatternKind::Binding(bname) = &pattern.kind else {
            return;
        };
        let ExprKind::Tuple(elems) = &value.kind else {
            return;
        };
        // B-2026-08-27-48 — same method-frame guard as the siblings.
        if self.owned_param_frame_is_method.last().copied() == Some(true) {
            return;
        }
        let Some(Value::Tuple(bound)) = self.env.get(bname) else {
            return;
        };
        let mut views: Vec<usize> = Vec::new();
        for (i, e) in elems.iter().enumerate() {
            let Some(ev) = bound.get(i) else {
                return;
            };
            if !self.value_runs_user_drop(ev) {
                continue;
            }
            let is_view = matches!(&e.kind, ExprKind::Identifier(src)
                if self.owned_param_names_stack
                    .last()
                    .is_some_and(|params| params.contains(src.as_str())));
            if is_view {
                views.push(i);
            }
        }
        let bname = bname.clone();
        for i in views {
            // B-2026-09-01-3 — the SAME pair into two sets, the tuple peer
            // of the struct pair B-2026-08-29-47 writes. The mask says the
            // walk skips this element; the record says the element belongs
            // to the caller, which is what a later `let x = t.0;` has to ask
            // before registering a slot of its own. The mask alone cannot
            // answer it — an element moved out by an ordinary `let` lands
            // there too, and that one the destination genuinely owns.
            self.param_view_tuple_elems.insert((bname.clone(), i));
            self.moved_out_tuple_elem_bodies.insert((bname.clone(), i));
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
    /// B-2026-08-29-19 — does the variant constructor `value`, whose result was
    /// just bound to `bname`, carry ONLY payloads that are param VIEWS?
    ///
    /// Interpreter twin of codegen's `enum_ctor_payload_bodies_are_caller_owned`.
    /// Under caller-retains a value moved out of an owned param is a view whose
    /// `Drop` body the CALLER runs; B-2026-08-01-15 propagates that through a
    /// plain rebind and B-2026-08-29-17 through a match-arm rebind, but the
    /// constructor — the same move one level up — was never covered, so the
    /// fresh binding registered a Drop slot and the body ran TWICE. Both
    /// compiled backends did the same, which is why no A/B gate could see it.
    ///
    /// The per-payload admission test is deliberately
    /// [`Self::value_runs_user_drop`] over the BOUND value, because that is
    /// exactly what [`Self::run_enum_payload_user_drops_value`] — this
    /// backend's `__karac_dropelems_enum_<E>` — will visit. Codegen's twin asks
    /// its own `type_runs_user_drop`, the established pair. Each side asking the
    /// question its own walker asks is what makes the two agree by construction
    /// rather than by convention; asking a NARROWER question here would
    /// over-suppress and lose a body codegen still runs.
    ///
    /// EVERY visited payload must be a view, not merely one: `W2.Two(r, R { .. })`
    /// shares one walk between both slots, so suppressing wholesale would trade
    /// this double for a MISSING body on the fresh payload. The mixed case keeps
    /// its slot and stays wrong (B-2026-08-29-24).
    fn let_ctor_payloads_are_param_views(&self, bname: &str, value: &Expr) -> bool {
        let ExprKind::Call { callee, args } = &value.kind else {
            return false;
        };
        // The two constructor spellings, matching codegen's twin: `E.V(..)` and
        // bare `V(..)`.
        let variant = match &callee.kind {
            ExprKind::Identifier(n) => n.clone(),
            ExprKind::Path { segments, .. } => match segments.last() {
                Some(v) => v.clone(),
                None => return false,
            },
            _ => return false,
        };
        let Some(Value::EnumVariant {
            variant: bound_variant,
            data: EnumData::Tuple(payloads),
            ..
        }) = self.env.get(bname)
        else {
            return false;
        };
        // B-2026-08-29-24 — `Option` / `Result` USED to be excluded here to
        // match codegen: their payload TypeExpr is the enum's own generic
        // parameter, which `emit_enum_payload_user_drop_bodies_fn` skips, so
        // there was no codegen walker to withhold and fixing this side alone
        // turned an agreed defect into a run-vs-build divergence. The exclusion
        // is gone because the other half landed with it: codegen's `optres_*`
        // let-site now withholds through `optres_ctor_payloads_are_all_param_views`.
        // The two must keep moving together — a change to one of these gates
        // without the other re-opens exactly that divergence.
        // A struct-VARIANT constructor is not an `ExprKind::Call`, so only the
        // tuple form reaches here; requiring the bound variant to be the one
        // the syntax names keeps a shadowed constructor from being read as one.
        if bound_variant != variant {
            return false;
        }
        let mut visited_any = false;
        for (i, payload) in payloads.iter().enumerate() {
            if !self.value_runs_user_drop(payload) {
                continue;
            }
            let Some(arg) = args.get(i) else {
                return false;
            };
            let is_view = matches!(&arg.value.kind, ExprKind::Identifier(src)
                if self.owned_param_names_stack
                    .last()
                    .is_some_and(|params| params.contains(src.as_str())));
            if !is_view {
                return false;
            }
            visited_any = true;
        }
        visited_any
    }

    /// Drop any stale param-VIEW mark carried by the names this `let`
    /// re-binds, so [`Self::let_destructures_owned_param`] classifies the new
    /// generation on its own RHS. Twin of codegen's
    /// `clear_stale_param_view_marks`; see the call site in `eval_block_inner`
    /// for the measurement.
    ///
    /// Exempts the self-rebind `let r = r;`, whose RHS reads the very
    /// generation being cleared and is meant to inherit its view-ness.
    fn clear_stale_param_view_marks(&mut self, stmt: &Stmt) {
        let (pattern, value) = match &stmt.kind {
            StmtKind::Let { pattern, value, .. } | StmtKind::LetElse { pattern, value, .. } => {
                (pattern, value)
            }
            _ => return,
        };
        let self_rebind = match &value.kind {
            ExprKind::Identifier(src) => Some(src.as_str()),
            _ => None,
        };
        let names: Vec<String> = pattern
            .binding_names()
            .into_iter()
            .filter(|n| self_rebind != Some(n.as_str()))
            .collect();
        if names.is_empty() {
            return;
        }
        if let Some(top) = self.owned_param_names_stack.last_mut() {
            for n in &names {
                top.remove(n);
            }
        }
        for n in &names {
            self.param_view_struct_fields.retain(|(root, _)| root != n);
            self.param_view_tuple_elems.retain(|(root, _)| root != n);
        }
    }

    /// B-2026-09-02-39, widened by B-2026-09-02-38 — the binding names a
    /// destructure pattern reaches: a `Struct` pattern's bound fields, or a
    /// `Tuple`/`TupleVariant` pattern's elements descending through nested
    /// `Tuple` elements.
    ///
    /// EACH HALF MIRRORS A DIFFERENT CODEGEN SITE, which is why one function
    /// with two shapes rather than one uniform walk. The tuple half mirrors
    /// `place_source_tuple_leaf_cleanups`, whose recursion descends into a
    /// `PatternKind::Tuple` element and no other kind. The struct half mirrors
    /// `finish_owned_struct_destructure`'s `bound_name`, which takes the
    /// shorthand `r`, takes `rr` from `r: rr`, and treats a wildcard or a
    /// nested pattern as unbound — so the struct half does NOT recurse, even
    /// though the tuple half does.
    ///
    /// The asymmetry is deliberate and measured against codegen rather than
    /// against symmetry: marking a name the compiled side never marks is how
    /// this family splits the backends.
    fn collect_destructure_binding_names(pattern: &Pattern, out: &mut Vec<String>) {
        // B-2026-09-02-38 — the STRUCT spelling (`let S { r, k } = s;`). Its
        // leaves are views of the callee's entry copy for the same reason the
        // tuple ones are, and the shape of "which name does this field bind"
        // is copied from codegen's `bound_name` in
        // `finish_owned_struct_destructure` so the two gates admit the same
        // set: the shorthand `r` binds `r`, `r: rr` binds `rr`, and anything
        // else — a wildcard, or a NESTED pattern — binds nothing here.
        //
        // Nested struct/tuple patterns are deliberately NOT recursed into,
        // which is the opposite of the `Tuple` arm below and matches codegen
        // rather than symmetry: its struct path registers DISPATCH for a
        // nested leaf and explicitly leaves per-leaf cleanup as a tracked
        // narrow leak, so a nested leaf never reaches a marking site there.
        // Marking one here alone would split the backends.
        if let PatternKind::Struct { fields, .. } = &pattern.kind {
            for f in fields {
                match &f.pattern {
                    None => out.push(f.name.clone()),
                    Some(p) => {
                        if let PatternKind::Binding(b) = &p.kind {
                            out.push(b.clone());
                        }
                    }
                }
            }
            return;
        }
        let elems: &[Pattern] = match &pattern.kind {
            PatternKind::Tuple(elems) => elems,
            PatternKind::TupleVariant { patterns, .. } => patterns,
            _ => return,
        };
        for p in elems {
            match &p.kind {
                PatternKind::Binding(b) => out.push(b.clone()),
                PatternKind::Tuple(_) => Self::collect_destructure_binding_names(p, out),
                _ => {}
            }
        }
    }

    /// B-2026-09-02-40 — the parameter name a tuple-destructure SOURCE is
    /// rooted at: the identifier itself (`let (r, k) = t;`), or the root of a
    /// pure `FieldAccess` chain (`let (r, k) = h.pe;` → `h`, `g.h.pe` → `g`).
    /// `None` for any other source shape.
    ///
    /// Walks `FieldAccess` ONLY, deliberately. Codegen's twin gate computes
    /// `owner_runs_bodies` from `place_root_ident`, which also reaches through
    /// `Index`/`TupleIndex` — but its marking site is additionally guarded by
    /// `place_field_chain_root`, written against this walk, because a container
    /// ELEMENT (`v[i].pe`) is not the callee's entry copy and no measurement
    /// backs treating its leaves as views.
    ///
    /// No caller-retains exclusion, and that is measured rather than assumed.
    /// `finish_place_source_tuple_destructure` opens with an
    /// `owned_struct_params` bail, so a param whose struct carries a direct
    /// `Vec`/`VecDeque`/`String` field looked like it would never reach
    /// codegen's marking site — which would have made marking it here a
    /// run-vs-build split. An exclusion mirroring that bail was written, and
    /// the probe refuted it: with `struct Hs { pe: (R, i64), name: String }`
    /// the COMPILED backends went to one body while the excluded interpreter
    /// stayed at two, i.e. the bail does not fire for this shape and the
    /// exclusion caused the very divergence it was meant to prevent. The
    /// `ownstr` cell of `e2e_projection_source_tuple_destructure_is_a_view` and
    /// its interpreter twin keep that measurement standing.
    /// B-2026-09-03-11 — remove the field a `moved_out_nested_field_bodies`
    /// path names, descending through the structs on the way.
    ///
    /// Removing the ENTRY is the whole mask, exactly as it is for the flat set
    /// one level up: `drop_user_drop_fields_of_value` resolves each declared
    /// field through a `get` and skips a missing one, so nothing has to be
    /// threaded through the walker's several other callers. Every field of
    /// every struct along the path other than the leaf is untouched and keeps
    /// its body.
    ///
    /// Silent on a path that does not resolve — a struct whose shape changed
    /// under a rebind, say. The mask is an optimisation of ownership, and
    /// declining to apply one is the pre-fix behaviour rather than a new
    /// failure mode.
    fn remove_field_at_path(v: &mut Value, path: &[String]) {
        let Value::Struct { fields, .. } = v else {
            return;
        };
        match path.split_first() {
            Some((leaf, [])) => {
                fields.remove(leaf.as_str());
            }
            Some((head, rest)) => {
                if let Some(inner) = fields.get_mut(head.as_str()) {
                    Self::remove_field_at_path(inner, rest);
                }
            }
            None => {}
        }
    }

    /// B-2026-09-03-11 — resolve a pure `FieldAccess` chain into its root
    /// binding and the field-NAME path from it (`g.h.pe` -> `("g", ["h", "pe"])`).
    ///
    /// Name-based where codegen's `projection_field_index_path` is index-based,
    /// which is the same split every other pair in this family uses: the
    /// interpreter masks by removing an entry from a `HashMap<String, Value>`,
    /// codegen by masking a field INDEX in a synthesized walker. `None` for any
    /// root that is not a plain identifier, so an element or a call result --
    /// neither of which has an owner this could hand back to -- keeps today's
    /// behaviour.
    fn field_chain_name_path(value: &Expr) -> Option<(String, Vec<String>)> {
        let ExprKind::FieldAccess { object, field } = &value.kind else {
            return None;
        };
        match &object.kind {
            ExprKind::Identifier(root) => Some((root.clone(), vec![field.clone()])),
            ExprKind::FieldAccess { .. } => {
                let (root, mut path) = Self::field_chain_name_path(object)?;
                path.push(field.clone());
                Some((root, path))
            }
            _ => None,
        }
    }

    fn destructure_source_param_root<'e>(&self, value: &'e Expr) -> Option<&'e str> {
        match &value.kind {
            ExprKind::Identifier(n) => Some(n.as_str()),
            ExprKind::FieldAccess { .. } => {
                let mut cur = value;
                loop {
                    match &cur.kind {
                        ExprKind::FieldAccess { object, .. } => cur = object,
                        ExprKind::Identifier(n) => return Some(n.as_str()),
                        _ => return None,
                    }
                }
            }
            _ => None,
        }
    }

    /// B-2026-09-02-17 — does this `let` / `let … else` bind out of a CONTAINER
    /// ELEMENT the container still owns (`let E.A(r) = v[0] else { … }`)?
    ///
    /// `v[i]` evaluates to `ref T` (design.md § The index operator), so a
    /// binding taken out of one is a VIEW of a defensive clone, not an owner:
    /// the container runs the payload's `Drop` body at its own NLL death and
    /// the binding owes none. That is B-2026-09-02-11's rule, and it is the
    /// answer `match v[i]`, `if let v[i]` and `while let v[i]` already give on
    /// every surface — each of them reaches it through
    /// `scrutinee_expr_is_consuming`, which has no `Index` arm and so answers
    /// false, standing the arm stash down.
    ///
    /// `let … else` never asked that question. It binds through `bind_pattern`
    /// directly and `push_drops_for_stmt` then registered a real slot per name,
    /// so the body ran TWICE: once via the container's still-armed walk
    /// (`disarm_moved_out_enum_payload_one` has no `Index` arm either, so
    /// nothing was retracted) and once via the binding's own slot. Measured
    /// `A dE dR3 got 4 dR3 B` under `--interp` against `A dE dR3 got 4 B` on
    /// all three compiled surfaces.
    ///
    /// THE ROOT TEST IS LOAD-BEARING, not a shape filter. Over a TEMPORARY
    /// container (`let E.A(r) = mkv()[0] else { … }`) nothing else owns the
    /// element, both backends already run exactly one body, and that body is
    /// the binding's — suppressing it there would lose it entirely rather than
    /// merely mistime it. `place_walk_is_retractable` is the same walk the
    /// disarm family uses to ask "is there an owner to hand back to": an
    /// identifier root (`v[0]`) or a one-hop field chain (`h.xs[0]`), and
    /// nothing else.
    ///
    /// Written against the STATEMENT rather than the spelling, so `let` and
    /// `let … else` answer one question. The plain-`let` spelling cannot reach
    /// it today — `let W { r, k } = v[0]` is `E_INDEX_MOVE_NON_COPY`
    /// (B-2026-08-26-21) — which makes the widening inert, and inert is the
    /// point: a spelling-dependent split is exactly how this family drifted
    /// apart in the first place.
    fn let_binds_borrowed_container_elem(stmt: &Stmt) -> bool {
        let value = match &stmt.kind {
            StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => value,
            _ => return false,
        };
        match &value.kind {
            ExprKind::Index { object, .. } => Self::place_walk_is_retractable(object),
            _ => false,
        }
    }

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
        // B-2026-08-27-48 — retraction is a HAND-OFF, valid only where the
        // caller fires instead, and a method frame's arguments reach no
        // caller-side fire. Bailing keeps every method frame registering its
        // slots; see `owned_param_frame_is_method` for the measurements, the
        // struct leg included.
        // B-2026-08-30-55 — the `let` spelling of the same question. Was a
        // blanket method-frame bail; now a frame that CLAIMED an argument holds
        // its slots (nothing else fires them) while one that claimed none
        // retracts to the caller, exactly as `record_assign_of_param_view`
        // does. The two spellings have to agree — that they do is the property
        // B-2026-08-29-58 restored, and keeping one predicate for both is what
        // stops them drifting apart again.
        if self
            .method_frame_caller_retains_args
            .last()
            .is_some_and(|caller_owns| !caller_owns)
        {
            return false;
        }
        // `PatternKind::Tuple` belongs on this list for the same reason
        // `Struct` does: `let (r, n) = p;` over an owned TUPLE param binds
        // views of the callee's entry copy, so registering Drop slots for
        // `r`/`n` fired the element's body a second time under `karac run
        // --interp` while both compiled backends fired only the caller's.
        // Measured on the two-fire repro: the extra fire came from the callee
        // block's `run_cleanup`, the surviving one from the caller's
        // `run_fresh_temp_arg_drops` — which is exactly codegen's
        // caller-retains fire, so dropping the callee slot leaves the backends
        // agreeing at one. `Slice` is deliberately NOT here: an array/slice
        // pattern over an owned param is a distinct ownership shape and no
        // measurement backs adding it.
        if !matches!(
            pattern.kind,
            PatternKind::Struct { .. } | PatternKind::TupleVariant { .. } | PatternKind::Tuple(_)
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
            // B-2026-08-29-19 — the CONSTRUCTOR spelling of the same move.
            // `let w = W.One(r);` stores a param view into a fresh local; the
            // caller still runs that payload's body, so registering a Drop slot
            // for `w` ran it twice. Propagates view-ness like the bare rebind
            // above, so `let w2 = w;` inherits it.
            if let PatternKind::Binding(bname) = &pattern.kind {
                if self.let_ctor_payloads_are_param_views(bname, value) {
                    let bname = bname.clone();
                    if let Some(top) = self.owned_param_names_stack.last_mut() {
                        top.insert(bname);
                    }
                    return true;
                }
            }
            // B-2026-08-29-47 — the move-out spelling, and the mirror image of
            // the constructor one above: `let x = s.r;` reads a caller-owned
            // value back OUT of the place a wrap put it into (or straight out
            // of a by-value param — `let x = o.r;` needs no wrap at all). The
            // caller runs that body, so registering a slot here ran it twice;
            // measured `dR1 dR1` against a due `dR1` on all four surfaces, with
            // a LOCAL source correct, which is what isolates view-ness rather
            // than the move-out machinery as the missing half.
            //
            // The method-frame bail at the top of this fn covers this arm too,
            // and has to: a method's arguments reach no caller-side fire, so
            // the interpreter's slot here is the ONLY one. Measured — with the
            // bail, `impl H { fn take(ref self, r: R) { let s = S1 { r: r };
            // let x = s.r; } }` keeps its single body and CONVERGES with
            // codegen, which this same row fixes from two down to one.
            if let PatternKind::Binding(bname) = &pattern.kind {
                if self.let_reads_param_view_field(value) {
                    let bname = bname.clone();
                    if let Some(top) = self.owned_param_names_stack.last_mut() {
                        top.insert(bname);
                    }
                    return true;
                }
            }
            return false;
        }
        // B-2026-09-02-40 — the source may be a bare parameter name OR a FIELD
        // CHAIN rooted at one (`let (r, k) = h.pe;`). Both name a tuple the
        // callee's entry copy owns; the projection spelling was simply never
        // admitted here, so it fell through to `push_drops_for_stmt` and minted
        // a second owner — `b12 dR12 dR12` against a due `b12 dR12`, agreed on
        // all four surfaces because codegen's own marking was gated to
        // identifiers purely to match this bail.
        let Some(n) = self.destructure_source_param_root(value) else {
            return false;
        };
        let n = n.to_string();
        if !self
            .owned_param_names_stack
            .last()
            .is_some_and(|params| params.contains(n.as_str()))
        {
            return false;
        }
        // B-2026-09-02-25 — retracting this destructure's OWN slots is only
        // half the hand-off. The names it binds are views of the callee's entry
        // copy too, and every sibling branch above says so by inserting into
        // `owned_param_names_stack`; this one returned `true` without it. So
        // `let (r, k) = t; let m = r;` failed `src_is_view` at the rebind and
        // minted a second owner — `b5 dR5 dR5` against a due `b5 dR5` — while
        // `match t { (r, k) => { let m = r; … } }` over the identical signature
        // was already correct (B-2026-08-31-7).
        //
        // THREE RESTRICTIONS, each one holding the two backends to the same
        // answer rather than trading an agreed-wrong cell for a divergence —
        // the trade this row's own opener declined. Every one is measured.
        //
        //  * TUPLE SPELLINGS ONLY — LIFTED by B-2026-09-02-38, which added the
        //    `Struct` spelling (`let S { r, k } = s;`). It was held back
        //    because `finish_owned_struct_destructure` TRANSFERS the body to
        //    the leaf instead of leaving it with the source, which would make a
        //    view mark a lie; measurement showed that transfer is gated on
        //    `var_owns_struct_field_bodies` and so happens for a LOCAL source
        //    and NOT for a param, whose body fires past the callee's own block
        //    on both backends exactly as the tuple spelling's does. `Tuple` and
        //    `TupleVariant` converge as before. `Slice` is still absent, with
        //    no measurement behind it.
        //  * A SEEDED PARAM SOURCE, not an inherited view — LIFTED by
        //    B-2026-09-02-44. The reason given was that codegen could not see
        //    through a tuple whole-rebind at all; that had gone stale, and the
        //    cited spelling (`t2.0.id`) measures one body on all four surfaces
        //    today. The single site that still declined was codegen's
        //    `owner_runs_bodies`, which tested `current_fn_param_names` rather
        //    than the `param_view_locals` union its own `expr_is_param_view`
        //    reads; widening it there let this propagation read the full
        //    `owned_param_names_stack`, which the containment test above has
        //    already required. Withholding it was ALSO not holding the two sides
        //    together, as the restriction claimed: the retraction above already
        //    fired for an inherited root, so `let h2 = h; let (r, k) = h2.pe;`
        //    with no trailing rebind was one body here against two compiled.
        //  * NESTED TUPLE ELEMENTS ARE INCLUDED, but only since B-2026-09-02-39.
        //    They were held back at first because a nested element of a by-value
        //    tuple param resolved to an EMPTY `TypeExpr` on the compiled side,
        //    so its leaf was never reached there and marking here alone split
        //    the backends — measured, `let ((r, a), b) = t; let m = r;` went to
        //    one body compiled and stayed at two interpreted. -39 registers the
        //    param's declared element types, which makes the compiled leaf
        //    reachable, and the two sides move together again.
        //
        // Nothing is held back here now. The propagation runs for any root the
        // containment test above admitted — a seeded param or a local that
        // inherited view-ness whole — which is the same set codegen's widened
        // `owner_runs_bodies` accepts, so the two gates admit one set rather
        // than agreeing by convention.
        let mut names: Vec<String> = Vec::new();
        Self::collect_destructure_binding_names(pattern, &mut names);
        if let Some(top) = self.owned_param_names_stack.last_mut() {
            top.extend(names);
        }
        true
    }

    /// B-2026-08-29-47 — does this `let` initializer read a field out of a
    /// place the CALLER owns? The interpreter twin of codegen's
    /// `field_move_out_source_is_param_view`, name-based where that one is
    /// index-based, and answering the same two questions in the same order:
    ///
    /// 1. Is the chain ROOT a param view — a by-value parameter of the running
    ///    function, or a local that inherited view-ness whole? Everything
    ///    reachable through a view is a view, so one test at the root settles
    ///    any depth (`w.s.r` as well as `s.r`).
    /// 2. Failing that, was the FIRST hop off the root recorded as a param-view
    ///    field? That covers the two wraps that never become views themselves —
    ///    a MIXED literal, which still owns its fresh fields, and a wrap over a
    ///    struct with its own `impl Drop`, whose body is the binding's own.
    ///    A view field's interior is caller-owned all the way down, so no
    ///    record is needed past the first hop.
    ///
    /// B-2026-09-01-3 — a hop is a struct FIELD or a TUPLE INDEX, and the walk
    /// takes both in one loop, so `o.t.0` and `w.s.r` settle at (1) alike and a
    /// mixed chain needs no special case. Which record answers (2) is decided by
    /// the KIND of the first hop, since a field name and an element index are
    /// held in separate stores: `param_view_struct_fields` and
    /// `param_view_tuple_elems`.
    ///
    /// The name predates the tuple arm and is deliberately kept — several
    /// ledger rows cite it, and renaming would break that grep trail.
    fn let_reads_param_view_field(&self, value: &Expr) -> bool {
        if !matches!(
            &value.kind,
            ExprKind::FieldAccess { .. } | ExprKind::TupleIndex { .. }
        ) {
            return false;
        }
        let mut cur = value;
        let mut first_hop: Option<FirstHop<'_>> = None;
        let root = loop {
            match &cur.kind {
                ExprKind::FieldAccess { object, field } => {
                    first_hop = Some(FirstHop::Field(field.as_str()));
                    cur = object;
                }
                ExprKind::TupleIndex { object, index } => {
                    first_hop = Some(FirstHop::Elem(*index as usize));
                    cur = object;
                }
                ExprKind::Identifier(n) => break n.as_str(),
                _ => return false,
            }
        };
        if self
            .owned_param_names_stack
            .last()
            .is_some_and(|params| params.contains(root))
        {
            return true;
        }
        match first_hop {
            Some(FirstHop::Field(f)) => self
                .param_view_struct_fields
                .contains(&(root.to_string(), f.to_string())),
            Some(FirstHop::Elem(i)) => self.param_view_tuple_elems.contains(&(root.to_string(), i)),
            None => false,
        }
    }

    /// B-2026-08-29-58 — the ASSIGNMENT spelling of the move
    /// [`Self::let_destructures_owned_param`] already handles for `let`.
    ///
    /// `match b { E.A(r) => { out = r; } .. }` over an OWNED enum param stores a
    /// payload VIEW of the caller's value into an outer local. The caller still
    /// owns that payload and runs its body, so leaving the target's own Drop
    /// slot armed ran it a SECOND time -- measured `dR0 mid dR8 dE dR8 v8`
    /// against `dR0 mid dE dR8 v8` on all three compiled surfaces. The `let`
    /// spelling of the same move (`let m = r;`) has been correct since
    /// B-2026-08-29-17 because it routes through the sibling's `src_is_view`
    /// test; an assignment target reached no such test.
    ///
    /// Silencing through `moved_out_user_drop_bindings` covers BOTH fires the
    /// target would otherwise run: the displaced-value drop at a LATER
    /// assignment (which consults the same set) and the binding's own slot at
    /// its live-range end. That is what makes the repeated-assignment shape
    /// (`out = r1; .. out = r2;`) agree with codegen, which likewise runs a
    /// displacement body only for a value the target genuinely owned.
    ///
    /// The view-ness PROPAGATES, exactly as the `let` leg propagates it, so a
    /// later `let z = out;` inherits the ownership story instead of registering
    /// a slot of its own.
    ///
    /// A METHOD frame is excluded for B-2026-08-27-48's reason, and the
    /// exclusion is load-bearing rather than defensive: a method's FRESH-TEMP
    /// argument reaches no caller-side fire at all today, so
    /// `t.take(E.A(R { .. }))` runs the payload body exactly ONCE -- from this
    /// very slot -- and retracting it would take that count to zero. Measured.
    /// The gap underneath is filed on its own row and is worth stating here
    /// because it is what makes the guard necessary: a method frame does not
    /// participate in the owned-ENUM-param protocol at all, so
    /// `impl T { fn eat(ref self, b: E) -> i64 { 3 } }` called with a fresh temp
    /// runs ZERO bodies on the interpreter against two on both compiled
    /// backends. The free-fn twin and the struct-param twin are both correct. The residue is that a method frame keeps running one body too
    /// many for a NAMED argument; that is deliberately left alone, because the
    /// `let` spelling has the identical residue in the identical frame, and the
    /// two spellings agreeing is exactly the property being restored here.
    fn record_assign_of_param_view(&mut self, target: &str, value: &Expr) {
        let ExprKind::Identifier(src) = &value.kind else {
            return;
        };
        // B-2026-08-30-55 — this was a blanket "never retract inside a method
        // frame", and the doc above records exactly why: a method frame's
        // argument reached no caller-side fire, so its own slot was the ONLY
        // owner and retracting took the body count to zero.
        //
        // That premise is now conditional. A method frame claims the arguments
        // nobody else can (fresh temps), and leaves the ones the caller still
        // fires. So ask WHICH: retract where the caller owns the value —
        // restoring the single-owner count the compiled backends produce — and
        // hold where this frame owns it, because there the slot really is the
        // only owner. Measured on both halves: lifting the guard outright fixed
        // the named spelling and lost the payload body on the fresh-temp one.
        // Asked at FRAME granularity, not per param, and deliberately so. The
        // source here is routinely a VIEW of the argument rather than the
        // argument itself — `match b { E.A(r) => { out = r; } }` assigns `r`,
        // which four separate sites propagate view-ness onto — so a per-name
        // test answers "not owned" for exactly the case the guard exists to
        // catch. A frame that claimed nothing has a caller firing every
        // argument, which is the licence the retraction needs.
        if self
            .method_frame_caller_retains_args
            .last()
            .is_some_and(|caller_owns| !caller_owns)
        {
            return;
        }
        let src_is_view = self
            .owned_param_names_stack
            .last()
            .is_some_and(|params| params.contains(src.as_str()));
        if !src_is_view {
            return;
        }
        if let Some(top) = self.owned_param_names_stack.last_mut() {
            top.insert(target.to_string());
        }
        self.moved_out_user_drop_bindings.insert(target.to_string());
    }

    /// B-2026-08-30-54 — the FIELD-target sibling of
    /// [`Self::record_assign_of_param_view`].
    ///
    /// `match b { E.A(r) => { h.f = r; } .. }` over an OWNED enum param stores
    /// a payload VIEW of the caller's value into a FIELD of an outer local. The
    /// caller still owns that payload and runs its body, so leaving the field
    /// in the base's field-bodies walk ran it a SECOND time -- measured
    /// `dR0 m dR8 dE dR8 v8` against every compiled backend's
    /// `dR0 m dE dR8 v8`.
    ///
    /// Routed through `moved_out_struct_field_bodies`, the PER-FIELD mask
    /// `drop_user_drop_fields_of_binding` already consults, rather than through
    /// the whole-binding `moved_out_user_drop_bindings` its identifier sibling
    /// uses. That is the difference the row named: a field is one leaf of the
    /// base's walk, so silencing the base wholesale would take every OTHER
    /// Drop-bearing field's body with it. The same set also gates the
    /// displaced-field fire above, which is what makes a REPEATED assignment
    /// (`h.f = r; .. h.f = R { .. };`) agree with codegen -- the displacement
    /// runs a body only for a value the field genuinely owned.
    ///
    /// View-ness does NOT propagate from here, and deliberately so: the
    /// identifier sibling propagates onto a NAME, and `h.f` is not one. A later
    /// `let z = h.f;` records its own move-out through the `let x = h.f`
    /// leg (B-2026-08-03-8), which is the same mask by a different route.
    ///
    /// The method-frame guard is [`Self::record_assign_of_param_view`]'s,
    /// verbatim and for its reason (B-2026-08-27-48 / B-2026-08-30-55): a frame
    /// that claimed its arguments is the only owner and must keep firing.
    fn record_field_assign_of_param_view(&mut self, base: &str, field: &str, value: &Expr) {
        let ExprKind::Identifier(src) = &value.kind else {
            return;
        };
        if self
            .method_frame_caller_retains_args
            .last()
            .is_some_and(|caller_owns| !caller_owns)
        {
            return;
        }
        let src_is_view = self
            .owned_param_names_stack
            .last()
            .is_some_and(|params| params.contains(src.as_str()));
        if !src_is_view {
            return;
        }
        self.moved_out_struct_field_bodies
            .insert((base.to_string(), field.to_string()));
    }

    /// B-2026-08-28-12 — run the user `Drop` bodies of every element/field a
    /// `let` destructure DISCARDS through a wildcard leaf.
    ///
    /// `let (_, n) = p;` and `let W { r: _, n } = w;` bind nothing for the
    /// wildcard position, so no Drop slot is ever registered for it and the
    /// discarded value's body ran zero times on every backend. One level up
    /// (`let _ = R { .. }`) has been correct since B-2026-07-30-11, and a
    /// `match` arm's wildcard is correct too, which is what shows this is a
    /// `let`-destructure hole rather than a rule about wildcards.
    ///
    /// GATED ON THE SOURCE, and that gate is the whole subtlety. A by-value
    /// PARAM source (`fn take(p: (R, i64)) { let (_, n) = p; }`) is already
    /// correct at ONE body: the caller owns the entry copy under
    /// caller-retains and fires it through `run_fresh_temp_arg_drops`, so
    /// firing here too would double it. That is exactly the distinction
    /// [`Self::let_destructures_owned_param`] already draws for BINDING
    /// leaves; this mirrors its two conditions rather than inventing a
    /// second gate, and deliberately does NOT mirror its view-PROPAGATION
    /// side effect — that runs once per statement in `eval_block_inner`,
    /// after this, and doing it twice would mark bindings this never saw.
    ///
    /// A method frame is excluded from the exclusion, for B-2026-08-27-48's
    /// reason: a method's arguments reach no caller-side fire, so there the
    /// leaf IS the only owner and must fire.
    fn run_wildcard_destructure_leaf_user_drops(
        &mut self,
        pattern: &crate::ast::Pattern,
        value: &Expr,
        val: &Value,
    ) {
        use crate::ast::PatternKind;
        // Mirror of `let_destructures_owned_param`'s two conditions, minus its
        // mutation. `false` for a method frame means "the leaf owns it", which
        // is why the early return is ordered this way.
        let caller_owns = self.owned_param_frame_is_method.last().copied() != Some(true)
            && matches!(&value.kind, ExprKind::Identifier(n)
                if self.owned_param_names_stack
                    .last()
                    .is_some_and(|params| params.contains(n.as_str())));
        if caller_owns {
            return;
        }
        let mut discarded: Vec<Value> = Vec::new();
        match (&pattern.kind, val) {
            (PatternKind::Tuple(pats), Value::Tuple(items)) => {
                // Recurses through NESTED tuple sub-patterns
                // (`let ((_, m), n) = p;`). Stopping at the top level answered
                // that shape differently from both compiled backends, whose
                // place-source walker already recursed — measured as a
                // run-vs-build divergence in the opposite direction from the
                // one this row fixed.
                fn collect(pats: &[Pattern], items: &[Value], out: &mut Vec<Value>) {
                    for (p, v) in pats.iter().zip(items.iter()) {
                        match (&p.kind, v) {
                            (PatternKind::Wildcard, _) => out.push(v.clone()),
                            (PatternKind::Tuple(inner), Value::Tuple(inner_items)) => {
                                collect(inner, inner_items, out);
                            }
                            _ => {}
                        }
                    }
                }
                collect(pats, items, &mut discarded);
            }
            (PatternKind::Struct { fields, .. }, Value::Struct { fields: vals, .. }) => {
                for fp in fields {
                    // `W { r: _, n }` — only the RENAMED form can carry a
                    // wildcard; the shorthand `W { r, n }` is a binding.
                    let Some(inner) = &fp.pattern else { continue };
                    if matches!(inner.kind, PatternKind::Wildcard) {
                        if let Some(v) = vals.get(&fp.name) {
                            discarded.push(v.clone());
                        }
                    }
                }
            }
            _ => return,
        }
        for v in discarded {
            self.run_discarded_value_user_drops(v.clone());
            // B-2026-08-28-40 — and an own-`impl Drop` ENUM leaf's live-variant
            // PAYLOAD bodies. The walk above runs such an enum's own body and
            // stops, so a discarded `E.A(R { .. })` leaf ran ONE body for the
            // TWO objects it holds, while a BOUND local of the same type runs
            // both on every backend — which is what says two is the target.
            //
            // Added HERE rather than inside `run_discarded_value_user_drops`
            // for the reason B-2026-08-28-39 established: that walker has ~31
            // callers and widening it moves shapes this fix has no measurement
            // for. Codegen's twin is the enum payload leg on
            // `emit_struct_user_drop_bodies_only_fn`, which likewise fires only
            // for an enum name.
            if let Value::EnumVariant { enum_name, .. } = &v {
                if self.program.drop_method_keys.contains_key(enum_name) {
                    self.run_enum_payload_user_drops_value(&v);
                }
            }
        }
    }

    /// B-2026-08-01-30 leg B — an index expression the displaced-bodies
    /// branch may safely evaluate ahead of the store's own evaluation:
    /// pure scalar arithmetic over literals / identifiers / casts only.
    /// The interp twin of codegen's `index_expr_is_pure_scalar` — the two
    /// gates must stay in step or the displaced body fires on one backend
    /// only.
    fn assign_index_is_pure_scalar(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Integer(_, _) | ExprKind::Identifier(_) | ExprKind::Bool(_) => true,
            ExprKind::Unary { operand, .. } => Self::assign_index_is_pure_scalar(operand),
            ExprKind::Cast { expr, .. } => Self::assign_index_is_pure_scalar(expr),
            ExprKind::Binary { left, right, .. } => {
                Self::assign_index_is_pure_scalar(left) && Self::assign_index_is_pure_scalar(right)
            }
            // The typechecker desugars primitive-int operators into intrinsic
            // calls (`base - 1` → `i64.sub(base, 1)`) before either backend
            // sees the AST — the Binary arm above never fires for them. Accept
            // exactly the registered primitive arithmetic/bit intrinsics over
            // pure operands; any user call keeps declining.
            ExprKind::Call { callee, args } => {
                let ExprKind::Path {
                    segments,
                    generic_args,
                } = &callee.kind
                else {
                    return false;
                };
                generic_args.is_none()
                    && segments.len() == 2
                    && matches!(
                        segments[0].as_str(),
                        "i8" | "i16"
                            | "i32"
                            | "i64"
                            | "u8"
                            | "u16"
                            | "u32"
                            | "u64"
                            | "usize"
                            | "isize"
                    )
                    && matches!(
                        segments[1].as_str(),
                        "add"
                            | "sub"
                            | "mul"
                            | "div"
                            | "rem"
                            | "neg"
                            | "bitand"
                            | "bitor"
                            | "bitxor"
                            | "shl"
                            | "shr"
                            | "not"
                    )
                    && args
                        .iter()
                        .all(|a| Self::assign_index_is_pure_scalar(&a.value))
            }
            _ => false,
        }
    }

    pub(crate) fn value_runs_user_drop(&self, value: &Value) -> bool {
        let Value::Struct { name, fields } = value else {
            return false;
        };
        self.program.drop_method_keys.contains_key(name)
            || fields
                .values()
                .any(|v| self.field_value_carries_user_drop(v))
    }

    /// Field-content half of [`Self::value_runs_user_drop`]: does a struct
    /// FIELD's value carry user-Drop work the walk arms of
    /// `drop_user_drop_fields_of_value` can reach? Sees through one container
    /// level — Vec/VecDeque elements, tuple elements, Map/SortedMap values,
    /// Set/SortedSet elements — mirroring codegen's `type_runs_user_drop`
    /// container widening (B-2026-08-01-22 leg b + B-2026-08-02-18) so the
    /// two backends' gates classify identically at displacement/discard/
    /// element sites. TOP-LEVEL classification is unchanged: a bare
    /// Array/Tuple/Map value still classifies `false` through
    /// `value_runs_user_drop`, keeping the dedicated container walkers the
    /// sole firers for direct bindings.
    fn field_value_carries_user_drop(&self, v: &Value) -> bool {
        match v {
            Value::Struct { .. } => self.value_runs_user_drop(v),
            Value::Array(rc) => rc
                .read()
                .map(|g| g.iter().any(|e| self.value_runs_user_drop(e)))
                .unwrap_or(false),
            Value::Tuple(items) => items.iter().any(|e| self.value_runs_user_drop(e)),
            Value::Map(entries) => {
                let entries = entries.read().unwrap().clone();
                entries
                    .iter()
                    .any(|(_, val)| self.value_runs_user_drop(val))
            }
            Value::SortedMap(entries) => entries.values().any(|val| self.value_runs_user_drop(val)),
            Value::Set(items) => {
                let items = items.read().unwrap().clone();
                items.iter().any(|e| self.value_runs_user_drop(e))
            }
            Value::SortedSet(items) => items.keys().any(|k| self.value_runs_user_drop(&k.0)),
            // B-2026-08-03-1 — an Option/Result PAYLOAD is Drop-relevant
            // content like any other one-level container's. Built-in enums
            // only: a user enum's payload bodies ride their own declared-type
            // walk, and admitting them here would double-fire.
            Value::EnumVariant {
                enum_name, data, ..
            } if enum_name == "Option" || enum_name == "Result" => match data {
                EnumData::Unit => false,
                EnumData::Tuple(vs) => vs.iter().any(|v| self.value_runs_user_drop(v)),
                EnumData::Struct(m) => m.values().any(|v| self.value_runs_user_drop(v)),
            },
            // B-2026-08-28-46/-47 — a user enum that declares its OWN `Drop`
            // is Drop-relevant CONTENT, exactly as a struct with one is. Only
            // the own-body case is admitted: a payload's bodies do ride their
            // own declared-type walk, which is what the note above this arm
            // warns would double-fire, and that reasoning is untouched here.
            //
            // This is load-bearing beyond the walks it enables. The answer
            // also gates the move-out bookkeeping: with it false, a
            // `struct W { e: E }` was not drop-relevant, so destructuring `w`
            // never marked it moved and the parent's field walk still ran at
            // `w`'s death -- firing `E`'s body a second time on top of the
            // discard path's. Admitting the field here and teaching the walks
            // to run it have to land together for that reason.
            Value::EnumVariant {
                enum_name, data, ..
            } => {
                // B-2026-08-28-54 — a payload-carrying variant counts too, not
                // just an own `Drop`. This predicate gates the move-out
                // bookkeeping as well as the walks (see the note on the
                // enum-field arm), so it has to answer for exactly the set the
                // walks now run.
                self.program.drop_method_keys.contains_key(enum_name)
                    || match data {
                        EnumData::Unit => false,
                        EnumData::Tuple(vs) => vs.iter().any(|v| self.value_runs_user_drop(v)),
                        EnumData::Struct(m) => m.values().any(|v| self.value_runs_user_drop(v)),
                    }
            }
            _ => false,
        }
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
        // B-2026-08-29-33 — TAKEN, not borrowed: the mask applies to this level
        // only, so the recursion below into nested struct fields cannot inherit
        // it through a field-name collision.
        let payload_masked = self
            .pending_payload_masked_fields
            .take()
            .unwrap_or_default();
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
        // (field name, declared head type name, declared TypeExpr). The
        // DECLARED type is what gates the walk — see
        // `declared_field_type_head`. The full TypeExpr feeds the tuple and
        // Map/Set arms (B-2026-08-02-18), whose element/value types live in
        // the TE's structure rather than its head name.
        let declared: Vec<(String, Option<String>, TypeExpr)> = def
            .fields
            .iter()
            .map(|f| {
                (
                    f.name.clone(),
                    Self::declared_field_type_head(&f.ty),
                    f.ty.clone(),
                )
            })
            .collect();
        // The struct's own generic param names (B-2026-08-02-14): a field
        // whose declared head is one of these is type-ERASED at the decl —
        // see the scoping note below for why such fields now fire
        // value-driven.
        let generic_param_names: Vec<String> = def
            .generic_params
            .as_ref()
            .map(|gp| gp.params.iter().map(|p| p.name.clone()).collect())
            .unwrap_or_default();
        for (field, declared_head, declared_te) in declared.into_iter().rev() {
            let Some(field_value) = fields.get(&field).cloned() else {
                continue;
            };
            // B-2026-08-03-1 — an `Option[P]` / `Result[O, E]` FIELD: run the
            // live payload's bodies. `Option`/`Result` are built-ins with no
            // source `EnumDef`, so the declared-type walk below can never
            // reach them (the same reason the direct-binding path needs its
            // own instantiation-driven gate); the value-driven recursion is
            // what the discard path already uses for them. Codegen twin: the
            // `is_optres_field` arm of `emit_user_drop_field_bodies_fn`,
            // which hands the field slot to the tag-guarded payload walker.
            if let Value::EnumVariant { enum_name, .. } = &field_value {
                if enum_name == "Option" || enum_name == "Result" {
                    self.run_discarded_value_user_drops(field_value.clone());
                    continue;
                }
            }
            // B-2026-08-01-22 leg b — a Vec/VecDeque field of Drop-running
            // elements: fire each live element's bodies, forward order
            // (codegen's dropbodies vec loop iterates 0..len identically).
            // Declared-type gated like the struct arm below; one container
            // level (nested Vec[Vec[..]] is the recorded residual).
            if let Value::Array(rc) = &field_value {
                if matches!(declared_head.as_deref(), Some("Vec") | Some("VecDeque")) {
                    let elems: Vec<Value> = rc.read().map(|g| g.clone()).unwrap_or_default();
                    for e in elems {
                        match &e {
                            Value::Struct { name: tn, .. } => {
                                if self.program.drop_method_keys.contains_key(tn) {
                                    let tn = tn.clone();
                                    self.run_user_drop_body_only(&tn, e.clone());
                                }
                                self.drop_user_drop_fields_of_value(&e);
                            }
                            Value::EnumVariant { enum_name, .. }
                                if enum_name != "Option" && enum_name != "Result" =>
                            {
                                if self.program.drop_method_keys.contains_key(enum_name) {
                                    let tn = enum_name.clone();
                                    self.run_user_drop_body_only(&tn, e.clone());
                                }
                                self.run_enum_payload_user_drops_value(&e);
                            }
                            _ => {}
                        }
                    }
                }
                continue;
            }
            // B-2026-08-02-18 — a TUPLE field's Drop-running elements:
            // forward element order (codegen's __karac_dropelems_tuple
            // walker fires its struct-GEP targets in element order).
            // Declared-element-type gated, with the same own-generic-param
            // exception the struct arm below documents; struct elements
            // only, matching the codegen walker's scope (enum elements stay
            // silent on both backends — recorded residual).
            if let Value::Tuple(items) = &field_value {
                if let TypeKind::Tuple(elem_tes) = &declared_te.kind {
                    let pairs: Vec<(Value, TypeExpr)> = items
                        .iter()
                        .cloned()
                        .zip(elem_tes.iter().cloned())
                        .collect();
                    for (e, ete) in pairs {
                        // B-2026-08-03-7 — an `Option`/`Result` ITEM inside the
                        // tuple field (`W { p: (Option[Res], i64) }`). Only a
                        // DIRECT struct item was walked, so the payload's body
                        // was silent on both backends; codegen's twin reaches it
                        // because its tuple-field arm hands the slot to the
                        // element walker, which gained the Option/Result leg in
                        // B-2026-08-03-1. Built-ins have no source `EnumDef`, so
                        // the declared-head match below can never admit them —
                        // route through the shared value recursion instead,
                        // exactly as the direct Option/Result FIELD arm does.
                        if let Value::EnumVariant { enum_name, .. } = &e {
                            if enum_name == "Option" || enum_name == "Result" {
                                self.run_discarded_value_user_drops(e.clone());
                                continue;
                            }
                        }
                        // B-2026-08-28-47 — a user ENUM item inside a tuple
                        // FIELD (`struct W { p: (E, i64) }`). The struct-shaped
                        // bind below skipped it, so the field stayed silent here
                        // while codegen's tuple walker ran the body. Same
                        // declared-element-head gate as the struct item path.
                        if let Value::EnumVariant { enum_name, .. } = &e {
                            let eh = Self::declared_field_type_head(&ete);
                            let is_own_param = eh
                                .as_deref()
                                .is_some_and(|h| generic_param_names.iter().any(|p| p == h));
                            if eh.as_deref() == Some(enum_name.as_str()) || is_own_param {
                                let tn = enum_name.clone();
                                // B-2026-08-28-54 — payload walk unconditional.
                                if self.program.drop_method_keys.contains_key(&tn) {
                                    self.run_user_drop_body_only(&tn, e.clone());
                                }
                                self.run_enum_payload_user_drops_value(&e);
                            }
                            continue;
                        }
                        let Value::Struct { name: tn, .. } = &e else {
                            continue;
                        };
                        let eh = Self::declared_field_type_head(&ete);
                        let elem_is_own_param = eh
                            .as_deref()
                            .is_some_and(|h| generic_param_names.iter().any(|p| p == h));
                        if eh.as_deref() != Some(tn.as_str()) && !elem_is_own_param {
                            continue;
                        }
                        if self.program.drop_method_keys.contains_key(tn) {
                            let tn = tn.clone();
                            self.run_user_drop_body_only(&tn, e.clone());
                        }
                        self.drop_user_drop_fields_of_value(&e);
                    }
                }
                continue;
            }
            // B-2026-08-02-18 — a Map/SortedMap field's VALUES
            // (Set/SortedSet ELEMENTS): the field twin of
            // `run_map_val_user_drops`, keyed off the struct def's declared
            // TE instead of the binding-name table.
            if matches!(
                &field_value,
                Value::Map(_) | Value::SortedMap(_) | Value::Set(_) | Value::SortedSet(_)
            ) {
                // Key bodies first, then value — B-2026-08-26-41, matching the
                // binding-death order.
                self.run_field_map_key_user_drops(&field_value, &declared_te, &generic_param_names);
                self.run_field_map_val_user_drops(&field_value, &declared_te, &generic_param_names);
                continue;
            }
            // B-2026-08-28-46 — an own-`Drop` user ENUM field. The dispatch
            // below is struct-shaped and answers `continue` for an enum, so
            // `let w = W { e: E.B, n: 1 }` ran NOTHING here while BOTH compiled
            // backends ran `E.drop`: codegen's field walker gained exactly this
            // arm in B-2026-08-28-40 and the interpreter did not follow. Same
            // order as that arm — the enum's own body, then the live variant's
            // payload bodies. `Option`/`Result` never reach here; they are
            // routed through `run_discarded_value_user_drops` at the top of
            // this loop. Declared-head gated like the struct arm below, with
            // the same bare-generic-param exception.
            if let Value::EnumVariant { enum_name, .. } = &field_value {
                let is_own_param = declared_head
                    .as_deref()
                    .is_some_and(|h| generic_param_names.iter().any(|p| p == h));
                if declared_head.as_deref() == Some(enum_name.as_str()) || is_own_param {
                    let tn = enum_name.clone();
                    // Own-`Drop` gate: see the note in the binding-level
                    // element loop -- codegen reaches an enum member only via
                    // `drop_method_keys`, so a payload-only enum must stay
                    // silent here too.
                    // B-2026-08-28-54 — payload walk unconditional, own body gated.
                    if self.program.drop_method_keys.contains_key(&tn) {
                        self.run_user_drop_body_only(&tn, field_value.clone());
                    }
                    // B-2026-08-29-33 — payload masked when a consuming arm
                    // over this field took it; the own body above is not.
                    // B-2026-08-29-36 — a ONE-element path masks here; a
                    // longer one belongs to a deeper level and is re-seeded
                    // for the nested walk below instead.
                    if !payload_masked
                        .iter()
                        .any(|p| p.as_slice() == [field.clone()])
                    {
                        self.run_enum_payload_user_drops_value(&field_value);
                    }
                }
                continue;
            }
            let Value::Struct {
                name: field_type, ..
            } = &field_value
            else {
                continue;
            };
            // SCOPING DECISION (B-2026-07-29-39, REVISED by B-2026-08-02-14):
            // the walk is DECLARED-type-driven — comparing the declared head
            // against the runtime struct name skips fields whose declared
            // type doesn't match the value. ONE exception now fires
            // value-driven: a field declared as a bare GENERIC PARAM of this
            // struct (`struct W[T] { r: T }` instantiated at a Drop type).
            // The original rule skipped it because codegen read only the
            // declared name and couldn't see through the erasure — firing
            // here would have broken run/build parity. Codegen's walk is now
            // mono-aware (`user_drop_field_indices_mono` resolves `T` through
            // the binding's instantiation), so BOTH backends fire and parity
            // holds in the firing direction instead of the silent-leak one.
            // Any other mismatch (a genuinely differently-typed value) keeps
            // the skip.
            let declared_is_own_param = declared_head
                .as_deref()
                .is_some_and(|h| generic_param_names.iter().any(|p| p == h));
            if declared_head.as_deref() != Some(field_type.as_str()) && !declared_is_own_param {
                continue;
            }
            if self.program.drop_method_keys.contains_key(field_type) {
                let field_type = field_type.clone();
                self.run_user_drop_body_only(&field_type, field_value.clone());
            }
            // B-2026-08-29-36 — thread a DEEPER mask one level down. The mask
            // is taken (not borrowed) at entry precisely so a nested walk
            // cannot inherit this level's by name collision; a path longer
            // than one hop is the case where it SHOULD travel, so re-seed it
            // explicitly with the head stripped. Codegen's twin is
            // `FieldSkipTree::nested`, consumed by the same recursion.
            let sub: std::collections::HashSet<Vec<String>> = payload_masked
                .iter()
                .filter(|p| p.len() > 1 && p[0] == field)
                .map(|p| p[1..].to_vec())
                .collect();
            if !sub.is_empty() {
                self.pending_payload_masked_fields = Some(sub);
            }
            self.drop_user_drop_fields_of_value(&field_value);
        }
    }

    /// B-2026-08-02-18 — run the user `impl Drop` bodies of a struct FIELD's
    /// Map/SortedMap values (Set/SortedSet elements) at the owner's death.
    /// The field twin of [`Self::run_map_val_user_drops`]: same declared-V
    /// gate and same per-value dispatch, but the declared TE comes from the
    /// struct def (via `drop_user_drop_fields_of_value`) rather than the
    /// binding-name table, and a bare-generic-param V fires value-driven
    /// (the B-2026-08-02-14 erasure exception — codegen's walker sees the
    /// subst-resolved TE, so both backends fire).
    /// B-2026-08-03-7 — run the Drop bodies of a TUPLE's items, one container
    /// level into each. The single walk behind every position that holds a
    /// tuple as CONTENT — a `Vec` element, a `Map`/`Set` value at the binding
    /// level, and a `Map`/`Set` value reached through a struct field — all of
    /// which previously handled only a DIRECT struct item and so went silent on
    /// `(Option[Res], i64)`. Forward order, matching codegen's struct-GEP order
    /// in `__karac_dropelems_tuple_*`. Bodies only: the tuple's heap is freed by
    /// whichever memory drop owns the container.
    pub(crate) fn run_tuple_item_user_drops(&mut self, items: Vec<Value>) {
        for it in items {
            if let Value::Struct { name: tn, .. } = &it {
                if self.program.drop_method_keys.contains_key(tn) {
                    let tn = tn.clone();
                    self.run_user_drop_body_only(&tn, it.clone());
                }
                self.drop_user_drop_fields_of_value(&it);
                continue;
            }
            // Built-ins have no source `EnumDef`, so no declared-head walk can
            // admit them — route through the shared value recursion the discard
            // path uses, exactly as the DIRECT Option/Result arms do.
            if let Value::EnumVariant { enum_name, .. } = &it {
                if enum_name == "Option" || enum_name == "Result" {
                    self.run_discarded_value_user_drops(it);
                    continue;
                }
                // B-2026-08-28-47 — a user ENUM item, for the same reason and
                // with the same dispatch as the binding-level element loop:
                // this walk is the one behind every position that holds a
                // tuple as CONTENT, so without it `struct W { p: (E, i64) }`
                // stayed silent here while codegen ran the body.
                // B-2026-08-28-54 — payload walk unconditional, own body gated.
                let tn = enum_name.clone();
                if self.program.drop_method_keys.contains_key(&tn) {
                    self.run_user_drop_body_only(&tn, it.clone());
                }
                self.run_enum_payload_user_drops_value(&it);
                continue;
            }
            if let Value::Tuple(inner) = &it {
                let inner = inner.clone();
                self.run_tuple_item_user_drops(inner);
            }
        }
    }

    fn run_field_map_val_user_drops(
        &mut self,
        field_value: &Value,
        declared_te: &TypeExpr,
        generic_param_names: &[String],
    ) {
        self.run_field_map_half_user_drops(field_value, declared_te, generic_param_names, false)
    }

    /// KEY-half twin of [`Self::run_field_map_val_user_drops`]
    /// (B-2026-08-26-41), for a `Map` held in a struct FIELD. Codegen twin:
    /// the `is_table_field` arm's `emit_map_key_user_drop_bodies_fn` call.
    fn run_field_map_key_user_drops(
        &mut self,
        field_value: &Value,
        declared_te: &TypeExpr,
        generic_param_names: &[String],
    ) {
        self.run_field_map_half_user_drops(field_value, declared_te, generic_param_names, true)
    }

    fn run_field_map_half_user_drops(
        &mut self,
        field_value: &Value,
        declared_te: &TypeExpr,
        generic_param_names: &[String],
        key_half: bool,
    ) {
        let vals: Vec<Value> = match field_value {
            // OBSERVABLE order, not storage order — see the note on
            // `run_map_half_user_drops`.
            Value::Map(entries) => entries
                .read()
                .unwrap()
                .iter_observable()
                .map(|(k, v)| if key_half { k.clone() } else { v.clone() })
                .collect(),
            Value::SortedMap(entries) => {
                if key_half {
                    entries.keys().map(|k| k.0.clone()).collect()
                } else {
                    entries.values().cloned().collect()
                }
            }
            // A Set's element IS the key half — walked as the value; the key
            // pass declines so it does not fire each element's body twice.
            Value::Set(items) if !key_half => {
                items.read().unwrap().iter_observable().cloned().collect()
            }
            Value::SortedSet(items) if !key_half => items.keys().map(|k| k.0.clone()).collect(),
            Value::Set(_) | Value::SortedSet(_) => return,
            _ => return,
        };
        let TypeKind::Path(p) = &declared_te.kind else {
            return;
        };
        let elem_idx = match p.segments.last().map(String::as_str) {
            _ if key_half => 0usize,
            Some("Set") | Some("SortedSet") => 0usize,
            Some("Map") | Some("SortedMap") => 1usize,
            _ => return,
        };
        let Some(crate::ast::GenericArg::Type(val_te)) =
            p.generic_args.as_ref().and_then(|a| a.get(elem_idx))
        else {
            return;
        };
        let declared_head = Self::declared_field_type_head(val_te);
        let val_is_own_param = declared_head
            .as_deref()
            .is_some_and(|h| generic_param_names.iter().any(|q| q == h));
        for v in vals {
            if let Value::Array(_) = &v {
                if matches!(declared_head.as_deref(), Some("Vec") | Some("VecDeque")) {
                    self.run_nested_array_struct_elem_bodies(&v);
                }
                continue;
            }
            // B-2026-08-03-1 — an Option/Result-valued Map (Set element):
            // value-driven payload recursion, matching the Option/Result arm
            // of codegen's `emit_map_val_user_drop_bodies_fn`.
            if let Value::EnumVariant { enum_name, .. } = &v {
                if enum_name == "Option" || enum_name == "Result" {
                    self.run_discarded_value_user_drops(v);
                    continue;
                }
            }
            // B-2026-08-03-7 — the TUPLE-valued sibling, silent here for the
            // same reason: a tuple TE has no declared head for the gate below.
            if let Value::Tuple(items) = &v {
                let items = items.clone();
                self.run_tuple_item_user_drops(items);
                continue;
            }
            let Value::Struct { name: tn, .. } = &v else {
                continue;
            };
            if declared_head.as_deref() != Some(tn.as_str()) && !val_is_own_param {
                continue;
            }
            let tn = tn.clone();
            if self.program.drop_method_keys.contains_key(&tn) {
                self.run_user_drop_body_on_value(&tn, v);
            } else if self.value_runs_user_drop(&v) {
                self.drop_user_drop_fields_of_value(&v);
            }
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
    pub(crate) fn run_user_drop_body_only(&mut self, type_name: &str, value: Value) {
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
            // B-2026-08-30-14 — a `Drop` body must be TRANSPARENT to a
            // control-flow signal that is already in flight.
            //
            // The block evaluator drains `pending_cf` into its own `Result`
            // (this file's `Ok(_) => self.pending_cf.take()`), and the call
            // below discards that `Result`. So running a body while a `return`
            // / `break` / `continue` was propagating CONSUMED the signal: the
            // `return` never happened, the loop ran to completion, and a
            // value-carrying `return` yielded unit. The body was lost too,
            // in the same motion: `eval_call` short-circuits on a set
            // `pending_cf` right after evaluating the callee and before
            // dispatching it, so every `println` in the body returned without
            // emitting. One cause, both symptoms -- which is why the body's
            // output vanished AND the statement after the `return` ran.
            //
            // Every OTHER drop-body site already satisfies this invariant by
            // construction: `run_cleanup` is reached only AFTER the block
            // evaluator has taken the signal out of `pending_cf`. The
            // fresh-temp scrutinee hooks in `eval_expr.rs` are the exception --
            // they can call in with it still set (measurably the `match` and
            // `if let` ones; the `while let` spelling stayed correct
            // throughout, which is pinned as a fixture row). Bracketing here rather than at those three sites makes the
            // invariant structural, so a fourth caller cannot forget it, and
            // costs nothing at the ~45 sites where the signal is already clear.
            let interrupted_cf = self.pending_cf.take();
            let _ = self.eval_body_growing(&body);
            self.env.pop_scope();
            if let Some(cf) = interrupted_cf {
                // `eval_body_growing` bottoms out in `eval_block_inner`, which
                // drains ANY signal the body itself raised into the `Result`
                // discarded on the line above -- so `pending_cf` is None here
                // and this restore is in practice unconditional. The
                // `is_unwind` test is a guard for the day that stops being
                // true, and it encodes the precedence `call_function` already
                // applies: a fault raised INSIDE the body outranks the
                // ordinary control flow it interrupted. It is deliberately NOT
                // load-bearing today -- a drop body that panics is a program
                // design.md § Drop and Destructors requires the effect checker
                // to REJECT outright ("no 'abort on panic in drop' fallback to
                // discover at runtime"), so teaching this path to propagate
                // one would be implementing the fallback the spec forbids
                // rather than fixing the missing static check.
                if !self.pending_cf.as_ref().is_some_and(ControlFlow::is_unwind) {
                    self.pending_cf = Some(cf);
                }
            }
        }
    }

    /// B-2026-08-28-51 — record that `expr` sits in an ESCAPING position (its
    /// value is handed to an owner rather than discarded) and push that
    /// property down through branch structure, so every arm tail of an
    /// escaping `if` / `if let` / `match` / block is escaping too.
    ///
    /// Seeded at the three escaping sites: a function body's tail, a `return`
    /// operand, and a `let` initializer. Escaping-ness is a STATIC property of
    /// a syntactic site, so growing the set on demand is equivalent to
    /// precomputing it with a full AST walker, and idempotent — the early
    /// return on an already-known site is what bounds the recursion.
    ///
    /// A DISCARDED `if` statement is deliberately not a seed: its arm tails go
    /// nowhere, and marking them would take a program that runs one body today
    /// to zero.
    pub(crate) fn note_escaping_site(&mut self, expr: &Expr) {
        if !self
            .cond_move_escaping_sites
            .insert((expr.span.offset, expr.span.length))
        {
            return;
        }
        match &expr.kind {
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
                if let Some(t) = &then_block.final_expr {
                    self.note_escaping_site(t);
                }
                if let Some(e) = else_branch {
                    self.note_escaping_site(e);
                }
            }
            ExprKind::Match { arms, .. } => {
                for arm in arms {
                    self.note_escaping_site(&arm.body);
                }
            }
            ExprKind::Block(b) => {
                if let Some(t) = &b.final_expr {
                    self.note_escaping_site(t);
                }
            }
            // B-2026-08-30-50 — a by-value call ARGUMENT is the fourth escaping
            // position; codegen's `note_escaping_site` carries the same arm and
            // the same gate. See it for why the MINTING test is load-bearing:
            // the seed disarms the binding the taken arm hands over, so it is
            // sound only where the argument temp then owns the value, and that
            // registration needs a minting tail to name a type from. An
            // ALL-PLACES wrapper has none, and seeding it dropped the taken
            // value's body on the compiled backends.
            ExprKind::Call { args, .. } | ExprKind::MethodCall { args, .. } => {
                for a in args {
                    if self.wrapper_has_minting_tail(&a.value) {
                        self.note_escaping_site(&a.value);
                    }
                }
            }
            _ => {}
        }
    }

    /// B-2026-08-28-51 — seed [`Self::note_escaping_site`] for the two
    /// ESCAPING STATEMENT positions, `let x = <expr>;` and `return <expr>;`.
    /// Runs as a pre-statement hook, before the statement evaluates, so the
    /// arm tails are already known by the time one of them is reached.
    pub(crate) fn note_escaping_stmt_sites(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            // B-2026-08-29-31 — a WILDCARD `let` is excluded, on exactly the
            // ground the `StmtKind::Expr` arm below already states for a
            // discarded `if`: "its arm tails go nowhere, and marking them
            // takes a program that runs one body today to zero". `let _ = ..`
            // has no destination either, so an arm tail naming an enclosing
            // local was marked ESCAPING, `record_conditional_move_tail`
            // silenced the local's own body, and nothing ran it — the value
            // was handed to a consumer that does not exist. The
            // BARE-STATEMENT spelling of the same branch was correct
            // throughout, which is what identifies the statement kind rather
            // than the branch as the unit. Codegen twin: the same exclusion in
            // its own `note_escaping_stmt_sites`.
            StmtKind::Let { pattern, value, .. }
                if !matches!(&pattern.kind, crate::ast::PatternKind::Wildcard) =>
            {
                self.note_escaping_site(value)
            }
            StmtKind::LetElse { value, .. } => self.note_escaping_site(value),
            // B-2026-08-30-50 — an ASSIGNMENT's RHS is an escaping position for
            // the same reason a `let`'s initializer is: the value goes to the
            // target rather than dying here.
            StmtKind::Assign { value, .. } => self.note_escaping_site(value),
            StmtKind::Expr(e) => {
                if let ExprKind::Return(Some(inner)) = &e.kind {
                    self.note_escaping_site(inner);
                }
                // B-2026-08-30-50 — a call in STATEMENT position
                // (`one(if c { b } else { mk(11) });`, result discarded). Its
                // arguments still escape INTO the call, so its wrapper
                // arguments need seeding exactly as they would inside a `let`;
                // without this the discarded spelling lost the minting arm's
                // body while the `let`-bound one ran it.
                //
                // The call, NOT the statement's expression generally: a
                // discarded `if` stays excluded (see `note_escaping_site`) --
                // its arm tails go nowhere, and marking them takes a program
                // that runs one body today to zero. A call's arguments are the
                // opposite: they have a consumer by construction.
                if matches!(&e.kind, ExprKind::Call { .. } | ExprKind::MethodCall { .. }) {
                    self.note_escaping_site(e);
                }
            }
            _ => {}
        }
    }

    /// B-2026-08-28-51 — the CONDITIONAL-MOVE half of
    /// [`Self::suppress_tail_expr_user_drop`], and the one case that family
    /// structurally cannot handle.
    ///
    /// A bare identifier at the tail of a branch ARM in escaping position is
    /// moved out on the path through its own arm and dies in place on every
    /// other path. The static retraction cannot express that: the arm's
    /// `cleanup` vector does not hold the binding — it was declared in an
    /// enclosing block — and retracting there on ALL paths would lose the body
    /// whenever a sibling arm runs. Nothing is suppressed today, so the
    /// enclosing drain fires the body a SECOND time on a value the caller
    /// already owns, and the caller then reads it: a double body plus a
    /// use-after-drop.
    ///
    /// Marking here IS the runtime bit that resolves it, for free: the
    /// interpreter evaluates only the TAKEN arm, so reaching this point is
    /// itself the proof that this path moved the value.
    ///
    /// Deliberately skips a binding THIS block owns. That one is already
    /// handled by the static retraction, whose scoping is what keeps it
    /// correct; marking it again would silence container walks the retraction
    /// leaves armed. Codegen's twin resolves the same bit with a per-binding
    /// `i1` drop flag, cleared in the arm's own basic block.
    /// B-2026-08-30-33 — disarm an adopted parameter's per-path body drop at a
    /// statement that hands the value over.
    ///
    /// [`Self::record_conditional_move_tail`] covers the BARE identifier at an
    /// escaping site, which is the conditionally-returned shape. It does not
    /// reach `return Some(r)`, `return inner(r)` or `let w = W { r: r };` — the
    /// value leaves inside an aggregate or a call — and those are exactly the
    /// shapes B-2026-08-30-28's guard had to decline because nothing disarmed
    /// them here. Measured before this: `return inner(r)` ran the body twice in
    /// the interpreter while codegen ran it once, a run-vs-build split on top of
    /// the double.
    ///
    /// Restricted to `cond_store_param_names`, so it can only touch a parameter
    /// a call actually registered a per-path drop for.
    fn disarm_cond_store_param_on_handover(&mut self, stmt: &Stmt) {
        if self.cond_store_param_names.is_empty() {
            return;
        }
        /// The same move shapes codegen's `hands_over` recognizes, so the two
        /// backends disarm on the same statements by construction.
        fn hands_over(e: &Expr, name: &str) -> bool {
            match &e.kind {
                ExprKind::Identifier(n) => n == name,
                ExprKind::Call { args, .. } | ExprKind::MethodCall { args, .. } => {
                    args.iter().any(|a| hands_over(&a.value, name))
                }
                ExprKind::StructLiteral { fields, .. } => {
                    fields.iter().any(|f| hands_over(&f.value, name))
                }
                ExprKind::Tuple(elems) => elems.iter().any(|el| hands_over(el, name)),
                _ => false,
            }
        }
        let handed: Option<&Expr> = match &stmt.kind {
            StmtKind::Let { value, .. } => Some(value),
            StmtKind::Assign { value, .. } => Some(value),
            StmtKind::Expr(e) => match &e.kind {
                ExprKind::Return(Some(inner)) => Some(inner),
                ExprKind::MethodCall { .. } | ExprKind::Call { .. } => Some(e),
                _ => None,
            },
            _ => None,
        };
        let Some(handed) = handed else { return };
        let hits: Vec<String> = self
            .cond_store_param_names
            .iter()
            .filter(|n| hands_over(handed, n))
            .cloned()
            .collect();
        for n in hits {
            self.moved_out_user_drop_bindings.insert(n);
        }
    }

    pub(crate) fn record_conditional_move_tail(&mut self, expr: &Expr, cleanup: &[CleanupAction]) {
        if !self
            .cond_move_escaping_sites
            .contains(&(expr.span.offset, expr.span.length))
        {
            return;
        }
        // B-2026-08-31-35 — every local the tail CONSUMES, not only one it
        // hands out whole. This read a bare `Identifier` and nothing else, so
        // `if c { t } else { u }` was right while the identical move one
        // aggregate deeper — `if c { W { r: t, b: 1 } } else { … }` — left `t`
        // armed and ran its body twice. `collect_aggregate_literal_sources` is
        // the same walker the container-move recorder uses and resolves a bare
        // identifier to itself, so the original behaviour is a special case of
        // this rather than a branch beside it. Codegen's twin
        // (`clear_cond_move_flags_for_tail_sources`) took the identical
        // widening, which is what keeps the two backends deciding this
        // together — the row filed it as an AGREED gap, and a one-sided fix
        // would have traded that for a run-vs-build divergence.
        let mut names: Vec<String> = Vec::new();
        Self::collect_aggregate_literal_sources(expr, &mut names);
        for name in names {
            if cleanup
                .iter()
                .any(|a| matches!(a, CleanupAction::Drop { name: n } if *n == name))
            {
                continue;
            }
            let type_name = match self.env.get(&name) {
                Some(Value::Struct { name, .. }) => name.clone(),
                // Enum-Drop parity — see `suppress_tail_expr_user_drop`.
                Some(Value::EnumVariant { enum_name, .. }) => enum_name.clone(),
                _ => continue,
            };
            if !self.program.drop_method_keys.contains_key(&type_name) {
                continue;
            }
            self.moved_out_user_drop_bindings.insert(name);
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
        // B-2026-09-02-2 — every source the escaping expression CONSUMES, not
        // only one it hands out whole. This read a bare `Identifier` and
        // nothing else, so `return r` was right while the identical move one
        // aggregate deeper -- `return W { r: r }` -- left `r` armed and ran its
        // body twice: `mid dR14 v14 dR14 post` from `--interp` against
        // `mid v14 dR14 post` from all three compiled backends.
        //
        // The TUPLE spelling of the same move was already correct, which is
        // what pinned the cause. It reaches a SECOND channel: the escaping
        // positions also call `record_container_bodies_move_sources`, whose
        // `Tuple`/`ArrayLiteral` arms route through
        // `record_container_move_sources_in_aggregate_arg` and put a source
        // carrying its own `impl Drop` on the whole-value channel
        // (`moved_out_user_drop_bindings`, B-2026-08-02-27 / B-2026-08-29-45),
        // while its `StructLiteral` arm deliberately keeps the container-only
        // recording. That split is right where it is written -- the whole-value
        // channel is wrong for the DISCARD position (`let _ = W { r: r0 }`),
        // where no struct-literal discard walk takes over the retracted body,
        // and both backends draw the line in the same place. So the struct
        // literal's escaping half belongs HERE, in the hook that only the two
        // escaping positions reach, rather than in that shared dispatcher.
        //
        // `collect_aggregate_literal_sources` resolves a bare identifier to
        // itself, so the original behaviour is a special case of this rather
        // than a branch beside it -- the same widening
        // `record_conditional_move_tail` took in B-2026-08-31-35, which is why
        // the ENCLOSING-block spelling (`if k > 0 { return W { r: r } }`) was
        // already correct: that path records through the conditional-move set
        // instead, and had been widened to aggregates.
        let mut names = Vec::new();
        Self::collect_aggregate_literal_sources(expr, &mut names);
        // `retract_user_drop_actions_for` applies the same two gates the
        // identifier-only body did: the source must resolve to a `Struct` or an
        // `EnumVariant` (Enum-Drop parity, B-2026-07-01-8) whose type declares
        // a user `impl Drop`. A container source (`Box3 { xs: xs }`) resolves
        // to `Value::Array` and is left alone, which is why the container
        // spelling -- correct before this -- stays correct.
        self.retract_user_drop_actions_for(&names, cleanup);
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
        // B-2026-08-29-30 — BOTH discard spellings. This was `let _ =` only,
        // matching a discard walk that was also `let _ =` only; now that the
        // bare statement admits a literal tail, its moved place elements need
        // the same retraction or the source's wrapper fires over a moved-from
        // slot. Wrapper-peeled, since `{ (r, 20) };` is the same discard.
        let value = match &stmt.kind {
            StmtKind::Let { pattern, value, .. }
                if matches!(pattern.kind, PatternKind::Wildcard) =>
            {
                Self::arm_tail_expr(value)
            }
            StmtKind::Expr(e) => Self::arm_tail_expr(e),
            _ => return,
        };
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
        // B-2026-09-01-18 — the literal may sit behind block wrappers
        // (`let _ = { W { r: t, b: 1 } };`). Peeled through
        // `discarded_struct_literal_tail`, so the wrapped spelling answers
        // exactly as the direct one does; the `Identifier` arm is deliberately
        // NOT peeled, since a wrapped bare name reaches its own hooks.
        let literal = match &value.kind {
            ExprKind::StructLiteral { .. } => Some(value),
            _ => Self::discarded_struct_literal_tail(value),
        };
        let source_names: Vec<String> = match (&value.kind, literal) {
            (ExprKind::Identifier(n), _) => vec![n.clone()],
            (_, Some(lit)) => match &lit.kind {
                ExprKind::StructLiteral { fields, .. } => fields
                    .iter()
                    .filter_map(|f| match &f.value.kind {
                        ExprKind::Identifier(n) => Some(n.clone()),
                        _ => None,
                    })
                    .collect(),
                _ => return,
            },
            _ => return,
        };
        self.retract_user_drop_actions_for(&source_names, cleanup);
    }

    /// Retract the `Drop` cleanup action of every named source whose value
    /// carries a user `impl Drop`, because the statement just moved that value
    /// somewhere that now owns it.
    ///
    /// Factored out of [`Self::suppress_let_rebind_user_drop`] so its
    /// bare-statement sibling
    /// [`Self::suppress_discarded_literal_source_user_drops`] applies the same
    /// rule rather than a second copy of it — the drift B-2026-08-29-20 had to
    /// repair once already on the `let _ =` / bare-statement pair.
    fn retract_user_drop_actions_for(
        &mut self,
        names: &[String],
        cleanup: &mut Vec<CleanupAction>,
    ) {
        for source_name in names {
            // Only suppress when the source's value has a user impl Drop.
            let type_name = match self.env.get(source_name) {
                Some(Value::Struct { name, .. }) => name.clone(),
                // Enum-Drop parity — see `suppress_tail_expr_user_drop`.
                Some(Value::EnumVariant { enum_name, .. }) => enum_name.clone(),
                _ => continue,
            };
            if !self.program.drop_method_keys.contains_key(&type_name) {
                continue;
            }
            cleanup.retain(|action| match action {
                CleanupAction::Drop { name } => name != source_name,
                _ => true,
            });
        }
    }

    /// B-2026-09-01-18 — the struct literal a DISCARDED statement owns, seen
    /// through any block wrappers around it.
    ///
    /// `suppress_let_rebind_user_drop` matches the literal SYNTACTICALLY at the
    /// `let`'s RHS, so it reaches `let _ = W { r: t, b: 1 };` and nothing else:
    /// a wrapper (`let _ = { W { .. } };`, `{ W { .. } };`) or a bare statement
    /// (`W { .. };`) all fell out of its `_ => return`. Those three ran the
    /// consumed local's body TWICE on this backend against once on both
    /// compiled ones, while the direct wildcard `let` — the one spelling it
    /// does reach — agreed. Peeling here is what makes the four answer alike.
    fn discarded_struct_literal_tail(e: &Expr) -> Option<&Expr> {
        match &e.kind {
            ExprKind::Block(b) | ExprKind::Seq(b) | ExprKind::Unsafe(b) => b
                .final_expr
                .as_deref()
                .and_then(Self::discarded_struct_literal_tail),
            ExprKind::StructLiteral { .. } => Some(e),
            _ => None,
        }
    }

    /// B-2026-09-01-18 — the BARE-STATEMENT sibling of
    /// [`Self::suppress_let_rebind_user_drop`]: `W { r: t, b: 1 };` and
    /// `{ W { r: t, b: 1 } };` construct a value nothing binds, so the
    /// statement itself owns it and runs its field bodies — and the local a
    /// field consumed must not run a second one at scope exit.
    ///
    /// Restricted to a STRUCT literal, which is the shape the `let` hook above
    /// covers and the shape measured to diverge. Tuple and array literals take
    /// the whole-value channel in `record_container_bodies_move_sources`
    /// instead, and were measured correct in every spelling of this family.
    fn suppress_discarded_literal_source_user_drops(
        &mut self,
        stmt: &Stmt,
        cleanup: &mut Vec<CleanupAction>,
    ) {
        let StmtKind::Expr(e) = &stmt.kind else {
            return;
        };
        let Some(tail) = Self::discarded_struct_literal_tail(e) else {
            return;
        };
        let mut names = Vec::new();
        Self::collect_aggregate_literal_sources(tail, &mut names);
        self.retract_user_drop_actions_for(&names, cleanup);
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
            // B-2026-08-01-19 — FieldAccess target (`o.h = h;`): the base
            // binding's Drop slot would fire the caller-retained value a
            // second time at o's death. Retract it — the caller keeps the
            // single fire. Same over-suppression trade as the codegen twin
            // (o's other Drop-bearing fields go silent); direct Identifier
            // bases only.
            if let ExprKind::FieldAccess { object, .. } = &target.kind {
                if let ExprKind::Identifier(base) = &object.kind {
                    cleanup.retain(|action| match action {
                        CleanupAction::Drop { name } => name != base,
                        _ => true,
                    });
                    return;
                }
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

    /// The value a match arm ultimately yields: an arm may be a bare
    /// expression or a block, and only the block's tail is the value. Mirrors
    /// codegen's `Codegen::block_tail_expr` so the two backends peel the same
    /// way when deciding a discarded match's ownership.
    fn arm_tail_expr(body: &Expr) -> &Expr {
        match &body.kind {
            ExprKind::Block(b) | ExprKind::Seq(b) | ExprKind::Unsafe(b) => {
                b.final_expr.as_deref().map_or(body, Self::arm_tail_expr)
            }
            _ => body,
        }
    }

    /// B-2026-08-29-31 — which of these arm bodies is the one that RAN?
    ///
    /// Matches `taken_branch_tail` against each candidate's peeled tail span.
    /// `None` means the record does not belong to THIS construct — a nested
    /// branch overwrote it, or none ran — and the caller falls back to judging
    /// every arm, which is what this backend did before the record existed.
    fn taken_arm_tail_of<'e>(&self, bodies: impl Iterator<Item = &'e Expr>) -> Option<&'e Expr> {
        let taken = self.taken_branch_tail?;
        bodies
            .map(Self::arm_tail_expr)
            .find(|t| (t.span.offset, t.span.length) == taken)
    }

    /// B-2026-08-31-35 — the tail of the arm that actually RAN, for a
    /// discarded branch this site is about to own.
    ///
    /// Peels the same three wrappers `discard_rhs_produces_owned_value` peels
    /// and asks [`Self::taken_arm_tail_of`], so "which arm ran" has one answer
    /// per backend rather than one per call site.
    fn taken_discarded_tail<'e>(&self, rhs: &'e Expr) -> Option<&'e Expr> {
        match &rhs.kind {
            ExprKind::Block(b) | ExprKind::Seq(b) | ExprKind::Unsafe(b) => b
                .final_expr
                .as_deref()
                .and_then(|e| self.taken_discarded_tail(e)),
            ExprKind::Match { arms, .. } => self.taken_arm_tail_of(arms.iter().map(|a| &a.body)),
            ExprKind::If {
                then_block,
                else_branch,
                ..
            } => self.taken_arm_tail_of(
                [then_block.final_expr.as_deref(), else_branch.as_deref()]
                    .into_iter()
                    .flatten(),
            ),
            _ => None,
        }
    }

    /// B-2026-08-31-35 — disarm every local the TAKEN arm's tail consumed,
    /// once this discard site has decided to own the value it produced.
    ///
    /// The interpreter twin of the codegen seed in `note_escaping_stmt_sites`.
    /// Both backends already disarm an arm tail's aggregate-literal sources on
    /// the path that hands them over; what neither did was reach that disarm
    /// for a DISCARDED branch, because a discarded statement is deliberately
    /// not an escaping site — its arm tails go nowhere WHEN NOTHING OWNS THE
    /// RESULT, which is the case the exclusion was written for. Where this
    /// site does own it, the local has handed its value over and a second body
    /// is one too many: `let t = mkd(7); let _ = if n == 0 { W { r: t, b: 1 } }
    /// else { .. };` printed `dD7 dD7` on every backend.
    ///
    /// Keyed on the arm that RAN, not on all arms: the sibling arm of a mixed
    /// branch never consumed anything and must keep its own body — which is
    /// why the codegen twin emits its store in the arm's own basic block
    /// rather than retracting statically.
    fn disarm_discarded_tail_sources(&mut self, rhs: &Expr) {
        let Some(tail) = self.taken_discarded_tail(rhs).cloned() else {
            return;
        };
        let mut names: Vec<String> = Vec::new();
        Self::collect_aggregate_literal_sources(&tail, &mut names);
        for name in names {
            let type_name = match self.env.get(&name) {
                Some(Value::Struct { name, .. }) => name.clone(),
                Some(Value::EnumVariant { enum_name, .. }) => enum_name.clone(),
                _ => continue,
            };
            if !self.program.drop_method_keys.contains_key(&type_name) {
                continue;
            }
            self.moved_out_user_drop_bindings.insert(name);
        }
    }

    /// B-2026-08-29-31 — record the tail of the branch arm now running; see
    /// [`crate::Interpreter::taken_branch_tail`]. `None` clears it, so a
    /// construct that yields no value cannot leave a stale span behind for the
    /// next discard site to read.
    pub(crate) fn note_taken_branch_tail(&mut self, tail: Option<&Expr>) {
        self.taken_branch_tail = tail.map(|t| {
            let t = Self::arm_tail_expr(t);
            (t.span.offset, t.span.length)
        });
    }

    /// B-2026-08-29-31 — may this discard site OWN what an arm tail hands out?
    ///
    /// The question is only interesting for a bare `Identifier` tail, and the
    /// answer turns on whether that name still resolves. Both arms of
    /// `discard_rhs_produces_owned_value` used to exclude an `Identifier` tail
    /// outright, which is right for one of the two populations and wrong for
    /// the other:
    ///
    ///   * an ENCLOSING LOCAL (`let r = ..; let _ = match n { 0 => r, .. };`)
    ///     is still live here, and since B-2026-08-29-31 stopped marking a
    ///     wildcard `let` as an escaping position it keeps its own scope-exit
    ///     body. Firing here as well would run that body TWICE.
    ///
    ///   * an arm's own PATTERN BINDING (`match o { Some(r) => r, .. }`) has
    ///     already left scope by the time this runs, and the arm's static
    ///     retraction (`suppress_tail_expr_user_drop`) took its slot when the
    ///     tail handed it out. Nothing owns it, so this site must.
    ///
    /// `env.get` separates them exactly, and it is the same liveness gate the
    /// BARE-STATEMENT `If` arm has always used — which is why the two
    /// statement forms now answer alike instead of one excluding a population
    /// the other admits.
    fn discard_arm_tail_is_ownable(&self, tail: &Expr) -> bool {
        match &tail.kind {
            ExprKind::Identifier(n) => self.env.get(n).is_none(),
            _ => true,
        }
    }

    /// B-2026-08-31-22 — does the DISCARD PRODUCER behind `expr` construct the
    /// enum inline, so the payload-bodies walk is this site's to run?
    ///
    /// The `&self` successor to B-2026-08-29-30's `discard_producer_expr`
    /// (removed with this row — this was its only caller), extended to the
    /// two-tail `if` and the `match`. Those were deliberately not peeled
    /// while the compiled statement discard site registered no payload walk for
    /// a branch of enum ctors — peeling then would have moved this backend from
    /// agreeing at ONE body to disagreeing at TWO. That site now registers it,
    /// so the peel is what keeps the backends together.
    ///
    /// ALL arms must qualify, which is the same rule codegen's
    /// `discard_arm_tail_qualifies` enforces and it is load-bearing for the
    /// same reason: the walk is VALUE-driven, so it runs over whichever arm's
    /// value actually arrived. Peel a construct with one producing arm and one
    /// handing out a LIVE LOCAL and the walk would fire over that local's
    /// payload, which its own binding still owns — a doubled body, not a
    /// missing one. An `Identifier` arm is therefore admitted only when the
    /// name no longer resolves, the same liveness test
    /// `discard_arm_tail_is_ownable` uses a few lines up.
    ///
    /// A UNIT-variant arm qualifies without being a construction: it carries no
    /// payload, so the value-driven walk finds nothing to run for it, and
    /// refusing it would decline the whole construct — which is precisely how
    /// the mixed `match n { 0 => E.A(mk(8)) _ => E.B }` came to run nothing on
    /// either compiled backend before this row.
    fn discard_producer_runs_payload_walk(&self, expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Block(b) | ExprKind::Seq(b) | ExprKind::Unsafe(b) => b
                .final_expr
                .as_deref()
                .is_some_and(|t| self.discard_producer_runs_payload_walk(t)),
            ExprKind::If {
                then_block,
                else_branch,
                ..
            } => match else_branch.as_deref() {
                // The no-`else` spelling keeps B-2026-08-29-30's behaviour: one
                // arm, judged on its own.
                None => then_block
                    .final_expr
                    .as_deref()
                    .is_some_and(|t| self.discard_producer_runs_payload_walk(t)),
                // `arm_tail_expr` on the else branch: it is a BLOCK, and an
                // unpeeled block matches none of the arms below — which
                // silently declined `else { E.B }`, the row's own repro.
                Some(e) => {
                    then_block
                        .final_expr
                        .as_deref()
                        .is_some_and(|t| self.discard_arm_yields_fresh_enum(t))
                        && self.discard_arm_yields_fresh_enum(Self::arm_tail_expr(e))
                }
            },
            ExprKind::Match { arms, .. } => {
                !arms.is_empty()
                    && arms
                        .iter()
                        .all(|a| self.discard_arm_yields_fresh_enum(Self::arm_tail_expr(&a.body)))
            }
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Path { .. } => true,
                // The bare spelling of the same construction.
                // B-2026-09-02-13 — …and a plain FUNCTION call that returns
                // one. This arm answered `false` for `let _ = mk(1);` because
                // `mk` is not a variant name, and the premise was accurate at
                // the time: codegen's discard registrar reached its enum
                // payload walker only on the no-own-`Drop` leg, so a call
                // returning an own-`Drop` enum got the wrapper alone and the
                // compiled backends ran the own body ALONE. That is the gate
                // this row removes, so the premise is gone with it — a call
                // producer now walks on every compiled surface, and declining
                // here would be the divergence rather than the agreement.
                //
                // Resolved through `user_fn_return_type_name`, which is the
                // interpreter's view of the same `fn_return_type_names` table
                // codegen's registrar looks the type up in, so the two admit
                // one population.
                ExprKind::Identifier(n) => {
                    self.find_enum_for_variant(n).is_some()
                        || self.user_fn_return_type_name(n).is_some()
                }
                _ => false,
            },
            // B-2026-09-01-31 — an enum STRUCT-VARIANT construction
            // (`let _ = Sv.Hold { inner: R { .. } }`). It is spelled as a
            // struct literal, so it matched no arm above and this predicate
            // answered `false`: the discard ran the enum's OWN body and never
            // its payload's, against `dSv dR1` on all three compiled surfaces.
            // The tuple-variant twin `Tv.A(r)` is an `ExprKind::Call` and was
            // correct in every discard shape, which is what localizes it.
            //
            // QUALIFIED only, exactly as B-2026-08-31-8's argument-side twin:
            // the unqualified `Hold { .. }` runs `dSv` here and NOTHING at all
            // on the compiled backends, so claiming it would deepen a
            // divergence rather than close one (B-2026-09-01-32).
            ExprKind::StructLiteral { path, .. } => {
                self.qualified_struct_variant_enum_name(path).is_some()
            }
            _ => false,
        }
    }

    /// One arm of the all-arms test in
    /// [`Self::discard_producer_runs_payload_walk`]: a construction, a unit
    /// variant, or a name that no longer resolves.
    fn discard_arm_yields_fresh_enum(&self, tail: &Expr) -> bool {
        if self.discard_producer_runs_payload_walk(tail) {
            return true;
        }
        match &tail.kind {
            ExprKind::Path { segments, .. } => segments.len() == 2,
            ExprKind::Identifier(n) => {
                self.env.get(n).is_none() && self.find_enum_for_variant(n).is_some()
            }
            _ => false,
        }
    }

    /// B-2026-09-01-11 — ask the arm that RAN, not every arm.
    ///
    /// [`Self::discard_producer_runs_payload_walk`] requires ALL arms to
    /// qualify, and the reason it gives is sound: the walk is value-driven, so
    /// a construct with one producing arm and one handing out a LIVE LOCAL
    /// would, on the second arm, walk a payload that local's binding still
    /// owns. What it does not need is to answer STATICALLY. The interpreter
    /// already records which arm tail produced the value
    /// (`note_taken_branch_tail`, B-2026-08-29-31, which
    /// `disarm_discarded_tail_sources` reads two lines from this site), so the
    /// hazard can be excluded per RUN instead of per SHAPE — which is how both
    /// compiled backends decide it, one arm's basic block at a time.
    ///
    /// The difference is exactly the row's shape: for
    /// `let _ = if c { E.A(mk(8)) } else { e };` with `c` true, all-arms
    /// declined over the `e` arm that never ran, so the interpreter printed the
    /// enum's own body and stopped while both compiled backends printed the
    /// payload's too. Asking the taken tail admits it, and on the run where `e`
    /// IS the value the same test declines — the local keeps its own body, and
    /// nothing is doubled.
    ///
    /// Falls back to the all-arms question when no arm tail was recorded (a
    /// non-branch producer, or a branch whose recorded span matched none of its
    /// arms), so nothing this predicate used to admit is lost.
    fn discard_taken_producer_runs_payload_walk(&self, expr: &Expr) -> bool {
        if self.discard_producer_runs_payload_walk(expr) {
            return true;
        }
        // …and ONLY where the all-arms question declined on a LIVE LOCAL. That
        // restriction is not caution, it is the difference between the two
        // reasons an arm can fail `discard_arm_yields_fresh_enum`, and only one
        // of them varies per run:
        //
        //   * a LIVE LOCAL is a hazard about THIS run — on the run where that
        //     arm is taken the binding still owns the payload, and on the run
        //     where it is not the arm contributed nothing. Asking the taken
        //     tail answers it exactly.
        //   * a CALL arm is not a hazard at all; it is a statement about what
        //     `let _ = mke(9);` does. That answer does not vary per run, so
        //     re-asking it per run buys nothing — which is why the call
        //     population is admitted by the STATIC predicate above
        //     (`discard_producer_runs_payload_walk`'s `Call` arm) rather than
        //     here.
        //
        //     B-2026-09-02-13 changed WHAT that answer is. It used to be "the
        //     own body ALONE on every backend", and this comment recorded the
        //     measurement that held the line: `let _ = if c { E.A(mk(8)) }
        //     else { mke(9) };` went to `dE dR8` here against `dE` on jit and
        //     aot. That was true of a codegen registrar which could not reach
        //     its enum payload walker for an own-`Drop` enum; it can now, so
        //     both spellings read `dE dR8` and the static arm admits calls.
        if !self.branch_arms_are_fresh_or_live_locals(expr) {
            return false;
        }
        self.taken_discarded_tail(expr)
            .is_some_and(|tail| self.discard_arm_yields_fresh_enum(tail))
    }

    /// Is every arm of this branch either a fresh producer or a name that is
    /// still LIVE — the one population
    /// [`Self::discard_taken_producer_runs_payload_walk`] re-asks per run?
    fn branch_arms_are_fresh_or_live_locals(&self, expr: &Expr) -> bool {
        let arm_ok = |t: &Expr| {
            let t = Self::arm_tail_expr(t);
            self.discard_arm_yields_fresh_enum(t)
                || matches!(&t.kind, ExprKind::Identifier(n) if self.env.get(n).is_some())
        };
        match &expr.kind {
            ExprKind::Block(b) | ExprKind::Seq(b) | ExprKind::Unsafe(b) => b
                .final_expr
                .as_deref()
                .is_some_and(|t| self.branch_arms_are_fresh_or_live_locals(t)),
            ExprKind::If {
                then_block,
                else_branch,
                ..
            } => {
                let then_ok = then_block
                    .final_expr
                    .as_deref()
                    .is_some_and(|t| arm_ok(t) || self.branch_arms_are_fresh_or_live_locals(t));
                then_ok
                    && else_branch
                        .as_deref()
                        .is_none_or(|e| arm_ok(e) || self.branch_arms_are_fresh_or_live_locals(e))
            }
            ExprKind::Match { arms, .. } => {
                !arms.is_empty()
                    && arms.iter().all(|a| {
                        arm_ok(&a.body) || self.branch_arms_are_fresh_or_live_locals(&a.body)
                    })
            }
            _ => false,
        }
    }

    /// B-2026-09-01-11 — the taken-arm form of the BARE-STATEMENT discard's
    /// liveness gate, and the sibling of
    /// [`Self::discard_taken_producer_runs_payload_walk`] one question earlier.
    ///
    /// That site's `hands_out_live_binding` / `owns` gates ask whether ANY arm
    /// hands out a live binding and decline the whole construct if one does.
    /// The hazard is real — an enclosing local owns its own scope-exit body, so
    /// firing here as well doubles it — but it belongs to the arm that RAN. A
    /// mixed `if c { E.A(mk(8)) } else { e };` was declined whole, so with `c`
    /// true the interpreter ran NOTHING for a value it had just minted and
    /// thrown away, against `dE dR8` on both compiled backends.
    ///
    /// `None` when no arm tail was recorded, which leaves the caller on its
    /// all-arms question: a no-`else` `if` that took no arm, or a producer that
    /// is not a branch at all.
    fn discarded_branch_taken_tail_is_ownable(&self, e: &Expr) -> Option<bool> {
        self.taken_discarded_tail(e)
            .map(|t| self.discard_arm_tail_is_ownable(Self::arm_tail_expr(t)))
    }

    /// B-2026-08-29-25 — the expression whose SHAPE decides a bare-statement
    /// discard (`{ mk(7) };`, `{ match n { … } };`). The value has already
    /// been evaluated from the whole statement; only the shape dispatch needs
    /// the wrapper peeled, which is exactly what codegen's discard gates do —
    /// they compile the whole expression and key the cleanup battery on the
    /// tail.
    ///
    /// One shape is deliberately NOT peeled to: a bare `Identifier` that is
    /// not a unit variant. `r;` is a moved local whose own scope-exit body
    /// still fires, so a discard fire on top would double it, and compiled is
    /// silent for the wrapped spelling too — declining keeps the pair agreed
    /// rather than trading one defect for a divergence.
    fn discard_stmt_shape_expr<'e>(&self, expr: &'e Expr) -> &'e Expr {
        let tail = Self::arm_tail_expr(expr);
        match &tail.kind {
            ExprKind::Identifier(n)
                if !std::ptr::eq(tail, expr) && self.fresh_bare_unit_variant_enum(n).is_none() =>
            {
                expr
            }
            _ => tail,
        }
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
            // B-2026-08-28-41 — a QUALIFIED unit variant (`let _ = E.B`). It is
            // a bare `Path`, which matched no arm here, so the discard was
            // declined and an own-`impl Drop` enum ran NO body at all. The BARE
            // spelling (`let _ = B`) was admitted by the `Identifier` arm above
            // all along, which is why only one of the two spellings looked
            // broken on this backend.
            //
            // The value drops correctly in every OTHER position — bound
            // (`let e = E.B`), as a fresh argument (`take(E.B)`), and through a
            // tuple wildcard leaf — all at one body on all three backends, which
            // is what identifies the discard site rather than unit variants
            // generally.
            ExprKind::Path { segments, .. } if segments.len() == 2 => self
                .qualified_enum_variant_is_unit(&segments[0], &segments[1])
                .unwrap_or(false),
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
                ExprKind::Identifier(n) => {
                    self.program
                        .items
                        .iter()
                        .any(|it| matches!(it, Item::Function(f) if &f.name == n))
                        // B-2026-08-28-39 — a BARE variant constructor
                        // (`let _ = A(R { .. })`, the unqualified spelling of
                        // `E.A(..)`). The fn lookup above cannot see it: `A` is
                        // a VARIANT, not a program function, so the whole
                        // discard was declined and the interpreter ran NO body
                        // at all — measured 0 against both compiled backends'
                        // 2. The QUALIFIED spelling was admitted by the `Path`
                        // arm above all along, which is what made this family
                        // look like a payload-only gap rather than, on this
                        // spelling, a total miss.
                        || self.find_enum_for_variant(n).is_some()
                }
                _ => false,
            },
            // B-2026-08-29-20 — a discarded `match` (`let _ = match n { 1 => {
            // R { .. } } .. };`). This dispatch had no `Match` arm, so the
            // wildcard-let spelling ran NO `Drop` body on this backend while
            // the BARE-STATEMENT spelling (`match n { .. };`) ran one — and
            // codegen was silent too, its own gate missing the same leg.
            //
            // Every arm's TAIL must qualify, peeled with `arm_tail_expr` (the
            // twin of codegen's `block_tail_expr`), and `Identifier` is
            // EXCLUDED rather than inherited from the arm above. That
            // exclusion is the whole subtlety and the reason this could not
            // simply recurse: `ExprKind::Identifier(_) => true` is far more
            // permissive than codegen's freshness gate, so recursing whole
            // would fire this backend on arm shapes
            // `discarded_match_value_tail` rejects — trading an agreed silence
            // for a run-vs-build divergence one spelling over. An arm that
            // hands out a BINDING is a different population with a different
            // answer (B-2026-08-29-5) and is deliberately left silent here.
            //
            // Empty `arms` yields `false` through `all`, matching codegen's
            // explicit `arms.is_empty()` early return.
            ExprKind::Match { arms, .. } => {
                if arms.is_empty() {
                    return false;
                }
                // B-2026-08-29-31 — judge the arm that RAN when it is known.
                // Judging every arm and taking the conservative answer is
                // wrong in both directions for a MIXED branch: requiring all
                // arms ownable loses a minting arm's body, admitting on any
                // arm doubles a live local's. Compiled decides this per PATH,
                // in the arm's own basic block; `taken_branch_tail` is the
                // same bit for this backend.
                if let Some(tail) = self.taken_arm_tail_of(arms.iter().map(|a| &a.body)) {
                    let tail = tail.clone();
                    return self.discard_arm_tail_is_ownable(&tail)
                        && self.discard_rhs_produces_owned_value(&tail, val);
                }
                arms.iter().all(|a| {
                    let tail = Self::arm_tail_expr(&a.body);
                    self.discard_arm_tail_is_ownable(tail)
                        && self.discard_rhs_produces_owned_value(tail, val)
                })
            }
            // B-2026-08-29-25 — the `if` SPELLING of the arm above. An `if`
            // chooses between branch values exactly as a `match` does, and the
            // codegen twin decides both through one predicate
            // (`discarded_match_value_tail`), so the two must be admitted here
            // on identical terms — including the `Identifier` exclusion, whose
            // rationale is spelled out on the `Match` arm and applies verbatim
            // (a branch handing out a live enclosing local owns its own
            // scope-exit body; firing here would double it).
            //
            // A chosen block with no tail expression yields unit: there is no
            // owned value for this discard to be responsible for, and the twin
            // declines that for the same reason.
            //
            // B-2026-08-29-30 — an `if` with NO `else` is admitted, on the
            // then-tail alone. It was declined here (and by the twin) because
            // `compile_if`'s merge yields a const-0 placeholder when there is
            // no `else`, so no STATEMENT-site gate on either backend could
            // reach the arm's value — and firing on this backend alone would
            // have traded an agreed gap for a run-vs-build divergence. Codegen
            // now owns that value inside the ARM instead
            // (`discarded_arm_owned_aggregate_tail`), so the two backends can
            // fire together, which is what this admission is.
            //
            // The `Identifier` exclusion carries over verbatim and is doing
            // MORE work here than in the two-tail case: a no-`else` `if` whose
            // tail names a live enclosing local hands out a binding that owns
            // its own scope-exit body, and codegen's arm-level owner declines
            // that shape for exactly the same reason.
            //
            // When the branch is NOT taken the RHS evaluates to unit and the
            // walker this gate guards is value-driven, so a false positive
            // here costs nothing on that path.
            ExprKind::If {
                then_block,
                else_branch,
                ..
            } => match (then_block.final_expr.as_deref(), else_branch.as_deref()) {
                (Some(then_tail), else_tail) => {
                    // B-2026-08-29-31 — the taken arm when known; see the
                    // `Match` arm above for why all-arms conservatism is wrong
                    // for a mixed branch.
                    let candidates = [Some(then_tail), else_tail].into_iter().flatten();
                    if let Some(tail) = self.taken_arm_tail_of(candidates) {
                        let tail = tail.clone();
                        return self.discard_arm_tail_is_ownable(&tail)
                            && self.discard_rhs_produces_owned_value(&tail, val);
                    }
                    [Some(then_tail), else_tail]
                        .into_iter()
                        .flatten()
                        .map(Self::arm_tail_expr)
                        .all(|tail| {
                            self.discard_arm_tail_is_ownable(tail)
                                && self.discard_rhs_produces_owned_value(tail, val)
                        })
                }
                _ => false,
            },
            // B-2026-08-29-25 — a BLOCK-WRAPPED RHS (`let _ = { match .. }`,
            // `let _ = { mk(7) }`). This dispatch had no wrapper arm at all
            // while the codegen twins have peeled wrappers since slice 5, so
            // one pair of braces made this backend silent where all three
            // compiled surfaces fired — measured on a wrapped call, a wrapped
            // struct literal and a wrapped tuple before this landed.
            //
            // A bare `Identifier` tail is the exception, and it is not
            // symmetry for its own sake: `let _ = r` is admitted above only
            // because the let-rebind hook RETRACTS the local's own Drop slot,
            // making the discard fire the single body. No such hook fires
            // through a wrapper, so `let _ = { r }` would double it — and
            // compiled is silent there too, so declining keeps the pair
            // agreed. A bare unit VARIANT is not a place and stays admitted.
            ExprKind::Block(block) | ExprKind::Seq(block) | ExprKind::Unsafe(block) => block
                .final_expr
                .as_deref()
                .map(Self::arm_tail_expr)
                .is_some_and(|tail| {
                    !matches!(&tail.kind,
                        ExprKind::Identifier(n) if self.fresh_bare_unit_variant_enum(n).is_none())
                        && self.discard_rhs_produces_owned_value(tail, val)
                }),
            ExprKind::MethodCall { method, .. } => {
                matches!(
                    method.as_str(),
                    "insert"
                        | "remove"
                        | "swap_remove"
                        | "pop"
                        | "pop_back"
                        | "pop_front"
                        | "take"
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
    /// B-2026-09-02-12 — the payload bodies of a FRESH-TEMP `Option`/`Result`
    /// scrutinee that a pattern declined to match.
    ///
    /// `if let Ok(w) = mkerr()` builds a `Result[W, W]` holding `Err(W { .. })`,
    /// the arm does not take it, and the temporary dies right there — so `W`'s
    /// `Drop` body is due, exactly as it is for the `mkerr();` discard spelling
    /// one line away. It ran on NO surface: the miss edges reach
    /// [`Self::run_enum_payload_user_drops_value`], which is DECLARED-type
    /// driven and skips `Ok(T)` / `Err(E)` because those payloads are the
    /// enum's own generic parameters.
    ///
    /// The machinery that does see through the envelope is the `Option`/`Result`
    /// arm of [`Self::run_discarded_value_user_drops`] — VALUE-driven, so the
    /// concrete `W` in the variant is what it walks — and codegen's twin
    /// registrar `track_discarded_optres_payload_bodies` resolves the same
    /// payload through the instantiation the type-checker recorded. This routes
    /// the miss edges into that arm and nothing else, so a user enum keeps the
    /// declared-type walk it already had and nothing fires twice.
    /// Is this scrutinee a FRESH TEMPORARY — one whose result nothing else
    /// owns, so a declined match leaves it to die right there?
    ///
    /// B-2026-09-02-12's gate. A NAMED local reaching a miss edge keeps its own
    /// payload walk and runs the body at its own death, so firing there as well
    /// would double it; only a temporary has no other owner. Callers pair this
    /// with `scrutinee_expr_is_consuming`, which is what excludes the borrow
    /// accessors (`get` / `first` / `last`) whose `Option` payload ALIASES a
    /// container element the container still owns.
    pub(super) fn optres_freshtemp_scrutinee(e: &Expr) -> bool {
        matches!(e.kind, ExprKind::Call { .. } | ExprKind::MethodCall { .. })
    }

    pub(super) fn run_optres_payload_user_drops_value(&mut self, value: &Value) {
        let Value::EnumVariant {
            enum_name, data, ..
        } = value
        else {
            return;
        };
        if enum_name != "Option" && enum_name != "Result" {
            return;
        }
        let payloads: Vec<Value> = match data {
            EnumData::Unit => return,
            EnumData::Tuple(vs) => vs.clone(),
            EnumData::Struct(m) => m.values().cloned().collect(),
        };
        for v in payloads {
            self.run_discarded_value_user_drops(v);
        }
    }

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
            // B-2026-08-31-21 — a `shared` value fell to `_ => {}`, so every
            // discard spelling of a `shared` literal was silent on this backend
            // while the same value BOUND ran its body correctly.
            //
            // FIRED ON THE LAST REFERENCE, which is the model the bound
            // spelling already uses (`Env::drop_target` hands
            // `invoke_user_drop_if_applicable` an `Arc::strong_count` and it
            // fires at `== 1`). That is what settles the aliasing question this
            // row was held open for: a value reached here with one reference is
            // one nothing else can observe, so running the body IS the
            // 0-transition rather than a guess about aliases. Measured:
            // `let a = S { .. }; let b = a;` runs ONE body at scope end both
            // before and after this arm, and `let _ = a;` over a live binding
            // declines here (the binding holds the other reference) and keeps
            // running through its own path.
            Value::SharedStruct(ref inner)
                if self.program.drop_method_keys.contains_key(&inner.name)
                    && std::sync::Arc::strong_count(inner) == 1 =>
            {
                let tn = inner.name.clone();
                self.run_user_drop_body_on_value(&tn, val.clone());
            }
            _ => {}
        }
    }

    /// B-2026-08-31-21 — the `shared` leg of a discard, asked BEFORE the value
    /// is cloned into [`Self::run_discarded_value_user_drops`].
    ///
    /// That walker takes its argument by value and several callers hand it a
    /// CLONE, which bumps the `Arc` and defeats the last-reference test its
    /// `SharedStruct` arm performs — the same hazard `Env::drop_target`'s doc
    /// records for `get`. Callers that still hold the value ask this first, so
    /// the count they are judged on is the live one. Returns whether a body ran.
    fn run_discarded_shared_user_drop(&mut self, val: &Value) -> bool {
        let Value::SharedStruct(inner) = val else {
            return false;
        };
        if !self.program.drop_method_keys.contains_key(&inner.name)
            || std::sync::Arc::strong_count(inner) != 1
        {
            return false;
        }
        let tn = inner.name.clone();
        self.run_user_drop_body_on_value(&tn, val.clone());
        true
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
            // Recursive through NESTED literals (B-2026-08-02-23 leg 1) —
            // the codegen twin is `collect_aggregate_literal_sources`.
            //
            // B-2026-08-02-27 — the TUPLE arm routes through the consuming-ARG
            // helper so a source carrying its OWN `impl Drop` also lands on the
            // whole-value channel. `record_container_move_source_name` alone
            // skips such a struct (deferring to `suppress_let_rebind_user_drop`,
            // which only sees a BARE rebind), so `let r = Res{..}; let t = (r,
            // 9);` fired r's body at its own death AND again through the
            // tuple's element walk.
            // B-2026-08-29-45 — the ARRAY/`Vec` literal arm, missing entirely.
            // `let m = R { .. }; let v = [m];` armed the container's
            // element-body walk without retracting `m`'s own ownership, so the
            // body ran at `m`'s NLL death AND again through the walk —
            // `dR4 dR4` where one is due, on every backend, which is why no A/B
            // gate reported it. Routed through the whole-value channel like the
            // TUPLE arm beside it and for the same reason: the container's
            // element walk is the owner on both axes, so a source carrying its
            // OWN `impl Drop` must land on that channel rather than the
            // container-only one.
            ExprKind::Tuple(_)
            | ExprKind::ArrayLiteral(_)
            | ExprKind::PrefixCollectionLiteral { .. } => {
                self.record_container_move_sources_in_aggregate_arg(value)
            }
            // STRUCT literals keep the container-only recording, mirroring
            // codegen's split: the whole-value channel is wrong for the
            // WILDCARD position (`let _ = W { r: r0 }`), where no struct-literal
            // discard walk takes over the retracted body. Both backends must
            // draw this line in the same place or the discard position diverges.
            ExprKind::StructLiteral { .. } => {
                let mut names = Vec::new();
                Self::collect_aggregate_literal_sources(value, &mut names);
                for n in names {
                    self.record_container_move_source_name(&n);
                }
            }
            _ => {}
        }
    }

    /// Every bare-identifier source moved into an aggregate literal,
    /// RECURSIVELY through nested literals (B-2026-08-02-23 leg 1): the
    /// depth-1 walk saw only the outer literal's immediate fields, so
    /// `v.push(Outer { inner: Inner { xs: xs } })` never reached `xs`.
    /// Codegen twin: `collect_aggregate_literal_sources` in runtime.rs —
    /// the two must enumerate the same sources or the disarm diverges.
    pub(crate) fn collect_aggregate_literal_sources(e: &Expr, out: &mut Vec<String>) {
        match &e.kind {
            ExprKind::Identifier(n) => out.push(n.clone()),
            ExprKind::StructLiteral { fields, .. } => {
                for f in fields {
                    Self::collect_aggregate_literal_sources(&f.value, out);
                }
            }
            ExprKind::Tuple(elems) => {
                for el in elems {
                    Self::collect_aggregate_literal_sources(el, out);
                }
            }
            // B-2026-08-29-45 — recurse through an ARRAY/`Vec` literal too, so
            // a source nested inside one (`[Outer { r: m }]`) is reached for
            // the same reason nesting through a struct literal or a tuple is.
            ExprKind::ArrayLiteral(elems) => {
                for el in elems {
                    Self::collect_aggregate_literal_sources(el, out);
                }
            }
            // The `Vec[..]` / `Set[..]` PREFIX spelling is a distinct AST node
            // from the bare `[..]` one, so it needs its own arm: `let v: Vec[R]
            // = [m];` parses as this, not as `ArrayLiteral`, and handling only
            // the latter fixed the `Array[R, N]` annotation while leaving the
            // `Vec` one — the row's own repro — still doubling.
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for el in items {
                    Self::collect_aggregate_literal_sources(el, out);
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
            // B-2026-08-02-20 (leg 1) — Set/SortedMap/SortedSet were missing
            // here, so `let h = SetHold { s: s };` left the SOURCE binding's
            // element/value bodies walk armed and it fired early, at `s`'s
            // death rather than the holder's. Interp-only: codegen nulls the
            // source slot at the move, so its registered walker's null check
            // already no-ops.
            Some(
                Value::EnumVariant { .. }
                | Value::Array(_)
                | Value::Tuple(_)
                | Value::Map(_)
                | Value::Set(_)
                | Value::SortedMap(_)
                | Value::SortedSet(_),
            ) => {
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

    /// B-2026-08-29-15 — the OWN-`Drop` half of the passthrough disarm, for an
    /// argument the callee hands back AS ITSELF.
    ///
    /// [`Self::record_container_move_source_name`] deliberately skips an
    /// own-`Drop` struct, deferring to `suppress_let_rebind_user_drop`'s action
    /// retraction — which only sees a bare `let` rebind, so at a CALL ARG the
    /// binding stayed armed and fired alongside the caller's result binding:
    /// `let a = Res { .. }; let r = take(a);` ran the body twice for one
    /// object, agreeing on all four surfaces and so invisible to any A/B gate.
    ///
    /// Gated by the caller on the union of
    /// [`crate::ast::fn_always_returns_param`] and
    /// [`crate::ast::fn_conditionally_returns_param_bare`] — the two shapes in
    /// which some other frame is guaranteed to run the body on every path.
    /// This retracts the WHOLE-VALUE channel, so an argument that merely
    /// escapes SOMEWHERE is not enough: `fn_returns_param` alone would stand
    /// the caller down on a path where nobody else fires, the LOST body
    /// B-2026-08-28-22 measured.
    ///
    /// Same shape as the own-`Drop` widening
    /// [`Self::record_container_move_sources_in_aggregate_arg`] applies for
    /// B-2026-08-02-22, and it reuses that channel rather than adding one.
    pub(crate) fn record_returned_arg_user_drop_move(&mut self, name: &str) {
        let own_drop = matches!(self.env.get(name), Some(Value::Struct { name: ref tn, .. })
            if self.program.drop_method_keys.contains_key(tn));
        if own_drop {
            self.moved_out_user_drop_bindings.insert(name.to_string());
        }
    }

    /// B-2026-08-02-23 leg 2 (interp twin of the `flows_into_return` bodies
    /// leg in `call_dispatch`) — a bare-identifier arg in a position the callee
    /// RETURNS (`fn passthru(v) -> Vec[Res] { v }`, or a param moved into a
    /// returned aggregate literal) does not die at this call: the caller's
    /// consumer of the RESULT owns it. `run_fresh_temp_arg_drops` deliberately
    /// skips identifier args ("the caller binding's own NLL drop covers those")
    /// — correct when the value dies inside the callee, a duplicate body when
    /// it comes straight back out.
    ///
    /// Routed through `record_container_move_source_name`, which records only
    /// container/field walks and leaves an own-`Drop` struct binding armed —
    /// deliberately matching codegen's use of the CONTAINER-ONLY disarm there
    /// (an own-`Drop` struct param is entry-copied, so caller and callee hold
    /// genuinely distinct values).
    pub(crate) fn record_passthrough_arg_moves(
        &mut self,
        fn_name: &str,
        args: &[crate::ast::CallArg],
    ) {
        // B-2026-09-01-44 — resolved ONCE, and through the shared resolver
        // rather than the three inline `Item::Function` scans that used to sit
        // in the closure below. An ASSOCIATED callee reaches this function
        // under its bare method name, so those scans answered "not a
        // passthrough" for `H.apick(r, k)` while answering correctly for the
        // free-function twin `fpick(r, k)` — the caller then kept `r` armed and
        // the result binding ran the body a second time.
        let callee = self.callee_fn_for_param_ownership(fn_name);
        let passthrough: Vec<(String, bool)> = args
            .iter()
            .enumerate()
            .filter_map(|(i, arg)| {
                let ExprKind::Identifier(n) = &arg.value.kind else {
                    return None;
                };
                // B-2026-08-29-49 — the SECOND escape route for an
                // identifier arg, and the interp twin of the third clause in
                // `compile_call`'s gate. The walk this function exists to
                // correct (`run_fresh_temp_arg_drops`) consults
                // `fn_moves_param_into_outliving_place` for a FRESH-TEMP arg
                // and has since B-2026-08-26-9; an identifier arg is skipped by
                // that walk entirely and drops through its own binding, which
                // consulted nothing. So one callee, two spellings, two answers:
                // `take(mut sink, Res { id: 1 })` ran one body and
                // `take(mut sink, carg)` ran two — the binding's NLL fire at
                // the call plus the container's at its drain.
                let escapes_into_outliving_place =
                    callee.is_some_and(|f| crate::ast::fn_moves_param_into_outliving_place(f, i));
                // B-2026-08-09-15 — `fn_returns_param_payload` is the same
                // rule one level down: the callee hands back not the param but
                // something a `match` arm bound OUT of it, so the param reaches
                // no return site and `fn_returns_param` is blind to it while
                // the value still leaves the frame. Codegen's twin is
                // `callee_returns_enum_arg_payload`.
                let is_passthrough = callee.is_some_and(|f| {
                    crate::ast::fn_returns_param(f, i) || crate::ast::fn_returns_param_payload(f, i)
                });
                if !is_passthrough && !escapes_into_outliving_place {
                    return None;
                }
                // B-2026-08-29-15 / -50 — asked separately from the
                // passthrough gate above: the whole-value disarm below is
                // licensed only where SOME OTHER frame is guaranteed to run
                // the body on EVERY path. Two predicates give that guarantee,
                // and the union is the rule:
                //
                //  * `fn_always_returns_param` — every exit hands the argument
                //    back, so the caller's result binding owns it. This admits
                //    the returned AGGREGATE (`Hh { r: r }`) as well as the bare
                //    hand-back; see the predicate for why the aggregate's
                //    separate owner is parallel rather than nested.
                //  * `fn_conditionally_returns_param_bare` — each exit either
                //    hands it back (result binding owns it) or lets it die
                //    inside, where the CALLEE frame owns it via
                //    `cond_returned_param_drop_names`. Exactly one owner
                //    either way, which is why standing the caller down here is
                //    not the unconditional stand-down B-2026-08-28-22 measured
                //    a LOST body for.
                // The escape route carries the same guarantee the two return
                // predicates do — the value's new HOME runs the body — so it
                // licenses the whole-value disarm directly rather than only the
                // container-walk one.
                let callee_owns_body = escapes_into_outliving_place
                    || callee.is_some_and(|f| {
                        crate::ast::fn_always_returns_param(f, i)
                            || crate::ast::fn_conditionally_returns_param_bare(f, i)
                    });
                Some((n.clone(), callee_owns_body))
            })
            .collect();
        for (n, callee_owns_body) in passthrough {
            self.record_container_move_source_name(&n);
            if callee_owns_body {
                self.record_returned_arg_user_drop_move(&n);
            }
        }
    }

    /// Re-arm a name that just received a FRESH value (a new `let` binding or
    /// an assignment target): stale move-out records from a previous binding
    /// of the same name must not silence the new value's walks.
    pub(super) fn rearm_container_bodies_for_name(&mut self, name: &str) {
        self.moved_out_container_bodies_bindings.remove(name);
        self.moved_out_tuple_elem_bodies.retain(|(n, _)| n != name);
        // B-2026-09-03-11 — the deep sibling clears with the flat ones.
        self.moved_out_nested_field_bodies
            .retain(|(n, _)| n != name);
        self.moved_out_struct_field_bodies
            .retain(|(n, _)| n != name);
        self.moved_out_struct_field_payload_bodies
            .retain(|(n, _)| n != name);
        self.moved_out_tuple_elem_payload_bodies
            .retain(|(n, _)| n != name);
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

    /// B-2026-08-02-20 (leg 2) — a consuming method arg that is an AGGREGATE
    /// LITERAL (`v.push(Holder { xs: xs })`, `m.insert(k, (a, b))`) moves each
    /// source named in its fields/elements into the literal, which the
    /// container then owns. Disarm those sources' element/value bodies walks,
    /// exactly as the let-RHS sibling `record_container_bodies_move_sources`
    /// does for `let h = Holder { xs: xs };` — without it the element body
    /// printed twice, once at the source's death and once at the container's
    /// (parity-equal on both backends, but two fires for one logical value).
    ///
    /// Aggregate shapes ONLY: a bare-identifier arg keeps the existing
    /// whole-value channel (`record_ctor_arg_moves`), whose semantics differ
    /// (it disarms the source's OWN body too). Codegen twin: the
    /// StructLiteral / Tuple arms of `disarm_container_bodies_for_arg`.
    pub(crate) fn record_container_move_sources_in_aggregate_arg(&mut self, e: &Expr) {
        // B-2026-08-29-45 — the ARRAY/`Vec` literal admitted alongside the two
        // aggregate shapes. Without it the new dispatch arm in
        // `record_container_bodies_move_sources` reached here and bailed, and
        // `let m = R { .. }; let v = [m];` still ran `m`'s body twice.
        if !matches!(
            &e.kind,
            ExprKind::StructLiteral { .. }
                | ExprKind::Tuple(_)
                | ExprKind::ArrayLiteral(_)
                | ExprKind::PrefixCollectionLiteral { .. }
        ) {
            return;
        }
        let mut names = Vec::new();
        Self::collect_aggregate_literal_sources(e, &mut names);
        for n in names {
            self.record_container_move_source_name(&n);
            // B-2026-08-02-22 — a source carrying its OWN `impl Drop` needs
            // the whole-value channel as well: the container's element walk
            // owns its body now, and leaving the source armed fired it early
            // (at the source's death) in addition to the container's fire.
            // Codegen twin: the aggregate arms of
            // `disarm_container_bodies_for_arg` use `suppress_user_drop_for_var`,
            // which subsumes the container-element form the same way.
            let own_drop = matches!(self.env.get(&n), Some(Value::Struct { name: ref tn, .. })
                if self.program.drop_method_keys.contains_key(tn));
            if own_drop {
                self.moved_out_user_drop_bindings.insert(n);
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
    /// B-2026-09-03-15 — the instantiation an EXPRESSION produces, by the
    /// static half of [`Self::record_optres_payload_te`]'s chain: the
    /// span-keyed `enum_inst_type_exprs` a constructor records, else a called
    /// free function's declared return type.
    ///
    /// Split out because the tuple-element case needs exactly these two links
    /// and none of the others: there is no annotation to prefer (an element of
    /// a tuple literal carries none) and no source-var record to chain through
    /// (an element is not a binding).
    fn expr_instantiation_te(&self, e: &Expr) -> Option<TypeExpr> {
        if let Some(te) = self
            .program
            .enum_inst_type_exprs
            .get(&(e.span.offset, e.span.length))
        {
            return Some(te.clone());
        }
        match &e.kind {
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Identifier(f) => self.program.items.iter().find_map(|item| match item {
                    Item::Function(func) if func.name == *f => func.return_type.clone(),
                    _ => None,
                }),
                _ => None,
            },
            _ => None,
        }
    }

    /// B-2026-09-03-15 — record the element `TypeExpr`s of a tuple-bound
    /// variable, so a later `let (r, o) = t;` can resolve each leaf's type.
    /// The interpreter's twin of codegen's `tuple_var_elem_tes` registration,
    /// populated at the same moment (the binding's own `let`) and from the same
    /// two sources: the annotation when there is one, else the RHS tuple
    /// literal's elements.
    ///
    /// The bare-rebind arm (`let t2 = t;`) carries the record forward for the
    /// same reason `record_optres_payload_te`'s identifier arm does — the
    /// rebind names the same tuple, and codegen's `place_chain_tuple_tes`
    /// reaches it through the registry its own rebind path propagates.
    fn record_tuple_var_elem_tes(&mut self, name: &str, ty: &Option<TypeExpr>, value: &Expr) {
        if let Some(TypeExpr {
            kind: TypeKind::Tuple(elems),
            ..
        }) = ty
        {
            let elems = elems.iter().cloned().map(Some).collect();
            self.tuple_var_elem_tes.insert(name.to_string(), elems);
            return;
        }
        match &value.kind {
            ExprKind::Tuple(items) => {
                // B-2026-09-04-9 — through `destructure_elem_te`, so a NESTED
                // tuple element keeps its shape here too. Without it the
                // PLACE-source spelling of the nested destructure
                // (`let t = ((mk(6), Option.Some(mk(106))), 7);
                // let ((_, o), n) = t;`) read its element back as an opaque
                // `None` and lost the leaf, while the direct-literal spelling
                // of the same statement was correct — the two differ only in
                // which table the element type is read out of.
                let tes = items.iter().map(|e| self.destructure_elem_te(e)).collect();
                self.tuple_var_elem_tes.insert(name.to_string(), tes);
            }
            ExprKind::Identifier(src) => {
                if let Some(tes) = self.tuple_var_elem_tes.get(src.as_str()).cloned() {
                    self.tuple_var_elem_tes.insert(name.to_string(), tes);
                } else {
                    self.tuple_var_elem_tes.remove(name);
                }
            }
            _ => {
                self.tuple_var_elem_tes.remove(name);
            }
        }
    }

    /// B-2026-09-03-15 — the element `TypeExpr`s a tuple-destructure SOURCE
    /// offers, mirroring codegen's `place_chain_tuple_tes` link for link: the
    /// `let`'s own annotation, a bare identifier's registry entry, a tuple
    /// literal's element expressions, or a field chain's declared field type.
    ///
    /// The field-chain arm reads the object's struct NAME off its runtime
    /// value rather than from a static type table, which the interpreter has
    /// no equivalent of. That is not a value-driven gate creeping in: the name
    /// only selects which declaration to read, and the `TypeExpr` returned is
    /// the declared one — the same field type codegen pulls from
    /// `struct_field_type_exprs`.
    fn destructure_source_elem_tes(
        &self,
        ty: &Option<TypeExpr>,
        value: &Expr,
    ) -> Option<Vec<Option<TypeExpr>>> {
        if let Some(TypeExpr {
            kind: TypeKind::Tuple(elems),
            ..
        }) = ty
        {
            return Some(elems.iter().cloned().map(Some).collect());
        }
        match &value.kind {
            ExprKind::Identifier(src) => self.tuple_var_elem_tes.get(src.as_str()).cloned(),
            // B-2026-09-04-9 — a NESTED tuple literal element keeps its shape.
            // `expr_instantiation_te` answers for a value expression and has
            // nothing to say about `(mk(2), Option.Some(mk(102)))`, so the
            // element came back `None` and the nested-pattern recursion in
            // `record_destructure_optres_payload_tes` had no type to descend
            // into — the leaf's payload body ran on all three compiled surfaces
            // and on none here.
            //
            // An unresolvable SUB-element becomes the empty path rather than
            // collapsing the whole tuple to `None`: the recursion looks up
            // leaves positionally, so one unnameable sibling must not cost the
            // others their type. The empty path names nothing and is filtered
            // out downstream, which is the same fail-closed convention
            // codegen's element inference uses.
            ExprKind::Tuple(items) => {
                Some(items.iter().map(|e| self.destructure_elem_te(e)).collect())
            }
            ExprKind::FieldAccess { object, field } => {
                let obj = self.eval_place_type_name(object)?;
                let fields = self.program.items.iter().find_map(|item| match item {
                    Item::StructDef(s) if s.name == obj => Some(&s.fields),
                    _ => None,
                })?;
                let fte = fields.iter().find(|f| &f.name == field).map(|f| &f.ty)?;
                match &fte.kind {
                    TypeKind::Tuple(elems) => Some(elems.iter().cloned().map(Some).collect()),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// B-2026-09-04-9 — one tuple-literal element's type for
    /// [`Self::destructure_source_elem_tes`], preserving a NESTED tuple's shape
    /// so the leaf recursion can descend into it.
    fn destructure_elem_te(&self, e: &Expr) -> Option<TypeExpr> {
        if let ExprKind::Tuple(inner) = &e.kind {
            let inner_tes: Vec<TypeExpr> = inner
                .iter()
                .map(|x| {
                    self.destructure_elem_te(x).unwrap_or(TypeExpr {
                        kind: TypeKind::Path(crate::ast::PathExpr {
                            segments: vec![String::new()],
                            generic_args: None,
                            span: x.span,
                        }),
                        span: x.span,
                    })
                })
                .collect();
            return Some(TypeExpr {
                kind: TypeKind::Tuple(inner_tes),
                span: e.span,
            });
        }
        self.expr_instantiation_te(e)
    }

    /// B-2026-09-03-15 — the struct NAME a place expression's current value
    /// carries, for [`Self::destructure_source_elem_tes`]'s field-chain arm.
    fn eval_place_type_name(&self, e: &Expr) -> Option<String> {
        // B-2026-09-04-8 — resolve a CHAIN, not just a root. One hop was all
        // this answered, so `let (a, b) = g.h.inner;` asked it about `g.h`,
        // got `None`, and `destructure_source_elem_tes` bailed before naming a
        // single leaf — leaving the tuple's `Option`/`Result` element
        // unregistered and its payload body unrun, against all three compiled
        // surfaces which run it. `g.inner` was correct, which is what made the
        // gap look like a projection/local question rather than a DEPTH one.
        //
        // The root is resolved from the VALUE (a binding's runtime struct
        // name); each further hop from the DECLARATION, because that is what
        // the caller does with the answer anyway — it looks the name up in
        // `program.items` to read the field's tuple type. An unresolvable hop
        // still answers `None`, so an unnameable chain declines exactly as
        // before rather than guessing.
        if let ExprKind::FieldAccess { object, field } = &e.kind {
            let owner = self.eval_place_type_name(object)?;
            let fields = self.program.items.iter().find_map(|item| match item {
                Item::StructDef(s) if s.name == owner => Some(&s.fields),
                _ => None,
            })?;
            let fte = fields.iter().find(|f| &f.name == field).map(|f| &f.ty)?;
            return match &fte.kind {
                TypeKind::Path(p) => p.segments.last().cloned(),
                _ => None,
            };
        }
        let name = match &e.kind {
            ExprKind::Identifier(n) => n.as_str(),
            ExprKind::SelfValue => "self",
            _ => return None,
        };
        match self.env.get(name)? {
            Value::Struct { name, .. } => Some(name.clone()),
            _ => None,
        }
    }

    /// B-2026-09-03-15 — register each `Option`/`Result` LEAF of a tuple
    /// destructure in `optres_payload_bodies_tes`, the table
    /// `run_optres_payload_user_drops` consults at drop time.
    ///
    /// This is the interpreter half of the row, and it exists because
    /// `record_optres_payload_te` is called under `PatternKind::Binding` only:
    /// a destructure's leaves never reached a registration moment, so
    /// `let t = (mk(2), Option.Some(mk(22))); let (r, o) = t;` ran `dR2` and
    /// never `dR22`, while the same tuple left undestructured ran both.
    ///
    /// GATED ON THE OWNED-PARAM QUESTION, and on the same predicate
    /// `let_destructures_owned_param` uses for its `Tuple` arm rather than a
    /// paraphrase of it: when the source is a tuple the CALLER still owns, the
    /// caller runs the payload's body and registering here would fire it twice.
    /// That is codegen's `owner_runs_bodies` gate, so the two backends admit
    /// the same leaves.
    fn record_destructure_optres_payload_tes(
        &mut self,
        pattern: &Pattern,
        ty: &Option<TypeExpr>,
        value: &Expr,
    ) {
        if !matches!(
            &pattern.kind,
            PatternKind::Tuple(_) | PatternKind::Struct { .. }
        ) {
            return;
        }
        if let Some(root) = self.destructure_source_param_root(value) {
            let root = root.to_string();
            if self
                .owned_param_names_stack
                .last()
                .is_some_and(|params| params.contains(root.as_str()))
            {
                return;
            }
        }
        // B-2026-09-03-24 — the two destructure shapes resolve a leaf's type
        // from different places, so they are collected into one list and share
        // the `Option` filter below rather than each growing a copy of it.
        //
        // A TUPLE leaf is positional, so its type comes from the SOURCE (the
        // annotation, the source variable's recorded element types, the literal's
        // element expressions). A STRUCT field leaf is named, so its type comes
        // from the DECLARATION — the same field type codegen reads out of
        // `struct_field_type_exprs`, which is why the pattern's own path is the
        // first place looked and the runtime value's struct name only the
        // fallback for a pathless pattern.
        // B-2026-09-03-22 — the third element says whether a `Result` head is
        // admissible for this arm, and it differs by arm because CODEGEN differs
        // by arm. The TUPLE leaf takes `Result` since that row; the STRUCT-FIELD
        // leaf still declines it (B-2026-09-03-33's deferral, and B-2026-09-04-1
        // measured against this fix and still split), so admitting it here would
        // run a body the compiled backends do not — the divergence this family's
        // rules forbid, and the one two existing fixtures pin as agreed-absent.
        let leaf_tes: Vec<(String, TypeExpr, bool)> = match &pattern.kind {
            PatternKind::Tuple(pats) => {
                let Some(elem_tes) = self.destructure_source_elem_tes(ty, value) else {
                    return;
                };
                // B-2026-09-04-9 — RECURSE into a nested tuple sub-pattern
                // (`let ((_, o), n) = ((mk(2), Option.Some(mk(102))), 3);`).
                // Taking only the top level answered that shape differently
                // from the compiled backends, whose nested-pattern recursion
                // reaches the same leaf: the payload's body ran on all three
                // compiled surfaces and on none here.
                //
                // The WILDCARD sibling was already correct, through a different
                // path — `run_discarded_destructure_user_drops`' `collect`
                // recurses — which is what made the gap look like a binding /
                // wildcard asymmetry rather than a depth one.
                //
                // Descends only where BOTH sides are tuples: an element whose
                // recorded type is not a tuple cannot name the sub-leaves, and
                // guessing one would register a payload walk against the wrong
                // type.
                fn collect_leaves(
                    pats: &[Pattern],
                    tes: &[Option<TypeExpr>],
                    out: &mut Vec<(String, TypeExpr, bool)>,
                ) {
                    for (idx, pat) in pats.iter().enumerate() {
                        let Some(Some(te)) = tes.get(idx) else {
                            continue;
                        };
                        match &pat.kind {
                            PatternKind::Binding(leaf) => {
                                out.push((leaf.clone(), te.clone(), true))
                            }
                            PatternKind::Tuple(inner) => {
                                if let TypeKind::Tuple(inner_tes) = &te.kind {
                                    let inner_tes: Vec<Option<TypeExpr>> =
                                        inner_tes.iter().cloned().map(Some).collect();
                                    collect_leaves(inner, &inner_tes, out);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                let mut out = Vec::new();
                collect_leaves(pats, &elem_tes, &mut out);
                out
            }
            PatternKind::Struct { path, fields, .. } => {
                let Some(sname) = path
                    .last()
                    .cloned()
                    .or_else(|| self.eval_place_type_name(value))
                else {
                    return;
                };
                let Some(decl) = self.program.items.iter().find_map(|item| match item {
                    Item::StructDef(sd) if sd.name == sname => Some(sd.fields.clone()),
                    _ => None,
                }) else {
                    return;
                };
                fields
                    .iter()
                    .filter_map(|fp| {
                        // `Some(Binding(x))` is `b: x`; `None` is the shorthand
                        // `b`, which binds the FIELD's own name. A wildcard
                        // (`b: _`) binds nothing and is deliberately not a leaf
                        // here — it has no name to register under, and its own
                        // divergence is filed separately.
                        let leaf = match fp.pattern.as_ref().map(|p| &p.kind) {
                            Some(PatternKind::Binding(x)) => x.clone(),
                            None => fp.name.clone(),
                            _ => return None,
                        };
                        let fte = decl
                            .iter()
                            .find(|f| f.name == fp.name)
                            .map(|f| f.ty.clone())?;
                        // B-2026-09-04-1 — `Result` joins `Option` for a
                        // struct-field leaf whose SOURCE codegen now owns: an
                        // owned local, a fresh call or literal, or a by-value
                        // param. A PROJECTION source (`let HoRes { a, b } =
                        // w.inner;`) stays declined on both backends — the
                        // compiled leaf there is a view the source's drop still
                        // owns, and that family has its own row.
                        Some((
                            leaf,
                            fte,
                            !matches!(value.kind, ExprKind::FieldAccess { .. }),
                        ))
                    })
                    .collect()
            }
            _ => return,
        };
        for (leaf, te, allow_result) in leaf_tes {
            let TypeKind::Path(p) = &te.kind else {
                continue;
            };
            // B-2026-09-03-22 — `Result` joins `Option` here, in lockstep with
            // codegen's leaf arm. This filter was `Option`-only for exactly as
            // long as the compiled side was: a boxed `Result` payload had no
            // memory owner, so taking the leaf there ran the due body and
            // leaked, and running it HERE alone would have been the divergence
            // this family's rules forbid. `track_inline_result_agg_payload_var`
            // gives the compiled side its owner, so both sides move together.
            let head = p.segments.last().map(String::as_str);
            if !(head == Some("Option") || (allow_result && head == Some("Result"))) {
                continue;
            }
            let runs = p.generic_args.as_ref().is_some_and(|args| {
                args.iter().any(|a| match a {
                    crate::ast::GenericArg::Type(t) => self.type_expr_runs_user_drop(t),
                    _ => false,
                })
            });
            if runs {
                self.optres_payload_bodies_tes.insert(leaf, te.clone());
            }
        }
    }

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
            // B-2026-08-26-41 — a Map qualifies on EITHER half. The gate read
            // the value arg alone, so `Map[K, i64]` with a dropping K never
            // armed and the key walk had nothing to resolve: the walk was
            // present and silent, the gate-before-walk shape B-2026-08-02-24
            // and B-2026-08-03-1 both hit. Codegen needs no equivalent — its
            // `register_map_val_bodies` tries both emitters and each declines
            // on its own.
            let elem_idxs: &[usize] = match p.segments.last().map(String::as_str) {
                Some("Map") | Some("SortedMap") => &[0, 1],
                Some("Set") | Some("SortedSet") => &[0],
                _ => return false,
            };
            elem_idxs.iter().any(|&elem_idx| {
                matches!(
                    p.generic_args.as_ref().and_then(|a| a.get(elem_idx)),
                    Some(crate::ast::GenericArg::Type(v)) if self.type_expr_runs_user_drop(v)
                        || self.te_vec_elem_runs_user_drop(v)
                        // B-2026-08-03-1 — an Option/Result-valued V. This is the
                        // REGISTRATION gate, not the walk: without it the walk
                        // (which now has its Option arm) never armed, the same
                        // gate-before-walk shape as B-2026-08-02-24.
                        || self.field_te_runs_user_drop(v, &mut Vec::new())
                )
            })
        });
        if qualifies {
            self.map_val_bodies_tes
                .insert(name.to_string(), te.expect("qualifies implies Some"));
        } else {
            self.map_val_bodies_tes.remove(name);
        }
    }

    /// B-2026-08-01-23 — does `te` name a Vec/VecDeque whose ELEMENT type
    /// runs a user Drop? The interp twin of codegen's map-walker gate
    /// widening; scoped to the Map-values registration so the general
    /// `type_expr_runs_user_drop` keeps its head-name semantics everywhere
    /// else.
    fn te_vec_elem_runs_user_drop(&self, te: &TypeExpr) -> bool {
        let TypeKind::Path(p) = &te.kind else {
            return false;
        };
        let Some(head) = p.segments.first() else {
            return false;
        };
        if head != "Vec" && head != "VecDeque" {
            return false;
        }
        match p.generic_args.as_ref().and_then(|ga| ga.first()) {
            Some(crate::ast::GenericArg::Type(inner)) => {
                self.type_expr_runs_user_drop(inner) || self.te_vec_elem_runs_user_drop(inner)
            }
            _ => false,
        }
    }

    /// The walk half: run each stored VALUE's user `impl Drop` body for a
    /// dying `Map` binding.
    ///
    /// OBSERVABLE order (`iter_observable`), not storage order — B-2026-08-27-7.
    /// design.md § Map now states that an unordered container destroys its
    /// elements in the same unspecified, per-process order it iterates them, so
    /// this walk must go through the same permutation every other read path
    /// does. It previously called `iter()` and walked STORAGE, which is
    /// insertion-ordered; that was written when the interpreter iterated in
    /// insertion order too, and B-2026-08-21-6's seeded walk (`iter_observable`)
    /// left it behind. The result was a single interpreter run printing
    /// `iter 1 3 5 6 4 2` and then `drop 1 2 3 4 5 6` over the SAME map —
    /// self-contradictory, and worse, a STABLE order the compiled backends
    /// never reproduce, so a program that came to depend on it looked correct
    /// under `karac run` and reordered under `karac build`.
    ///
    /// The two backends still produce DIFFERENT permutations of each other:
    /// this sorts by seeded hash, the compiled walk follows bucket placement.
    /// That is within spec and is the point — both are now unspecified and
    /// both vary per process, so the dependency surfaces on the first pair of
    /// runs instead of at the backend boundary. `SortedMap`/`SortedSet` are the
    /// escape hatch and DO agree exactly, on every seed (measured).
    /// Parity tests stay order-insensitive.
    /// Returns `true` when the binding resolved to a Map (walked or not) so
    /// the caller stops — a Map is not a `drop_target` shape.
    /// B-2026-08-01-23 — fire the Drop bodies of STRUCT elements reachable
    /// through nested arrays (`Vec[Vec[Res]]`, a Vec-valued Map value's
    /// elements), recursing through `Value::Array` layers. Struct elements
    /// only — the same base case as codegen's te-driven
    /// `emit_nested_vec_elem_bodies_fn`, so enum elements inside nested
    /// containers keep their prior silence on both backends (recorded
    /// residual). Forward order at every level.
    fn run_nested_array_struct_elem_bodies(&mut self, arr: &Value) {
        let Value::Array(rc) = arr else {
            return;
        };
        let elems: Vec<Value> = rc.read().map(|g| g.clone()).unwrap_or_default();
        for e in elems {
            match &e {
                Value::Struct { name: tn, .. } => {
                    if self.program.drop_method_keys.contains_key(tn) {
                        let tn = tn.clone();
                        self.run_user_drop_body_on_value(&tn, e.clone());
                    } else if self.value_runs_user_drop(&e) {
                        self.drop_user_drop_fields_of_value(&e);
                    }
                }
                Value::Array(_) => self.run_nested_array_struct_elem_bodies(&e),
                _ => {}
            }
        }
    }

    fn run_map_val_user_drops(&mut self, name: &str) -> bool {
        self.run_map_half_user_drops(name, false)
    }

    /// KEY-half twin of [`Self::run_map_val_user_drops`] (B-2026-08-26-41).
    /// Codegen twin: `emit_map_key_user_drop_bodies_fn`. Declines for
    /// `Set`/`SortedSet`, whose single element is already walked as the value
    /// half — running it again here would fire each element's body twice.
    fn run_map_key_user_drops(&mut self, name: &str) -> bool {
        self.run_map_half_user_drops(name, true)
    }

    fn run_map_half_user_drops(&mut self, name: &str, key_half: bool) -> bool {
        // SortedMap shares the walk (same declared-V gate) but not the
        // ordering question: its entries come out in KEY order, which is
        // seed-independent and identical on every backend — the defined order
        // design.md points at for code that needs one.
        let vals: Vec<Value> = match self.env.get(name) {
            Some(Value::Map(entries)) => entries
                .read()
                .unwrap()
                .iter_observable()
                .map(|(k, v)| if key_half { k.clone() } else { v.clone() })
                .collect(),
            Some(Value::SortedMap(entries)) => {
                if key_half {
                    entries.into_keys().map(|k| k.0).collect()
                } else {
                    entries.into_values().collect()
                }
            }
            // Set-elements leg (B-2026-07-30-11): the walked values are the
            // ELEMENTS — the key half of the same table shape. So the key pass
            // must decline, or each element's body fires twice.
            Some(Value::Set(items)) if !key_half => {
                items.read().unwrap().iter_observable().cloned().collect()
            }
            Some(Value::SortedSet(items)) if !key_half => items.into_keys().map(|k| k.0).collect(),
            Some(Value::Set(_)) | Some(Value::SortedSet(_)) => return true,
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
            _ if key_half => 0usize,
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
            // B-2026-08-01-23 — a Vec/VecDeque-valued V: fire the nested
            // elements' bodies (declared-type gated like the struct arm).
            if let Value::Array(_) = &v {
                if matches!(declared_head.as_deref(), Some("Vec") | Some("VecDeque")) {
                    self.run_nested_array_struct_elem_bodies(&v);
                }
                continue;
            }
            // B-2026-08-03-1 — an Option/Result-valued V at the BINDING
            // level. This is a separate walk from the struct-FIELD map arm
            // (`run_field_map_val_user_drops`), so it needed the arm
            // independently; codegen reaches both through the one
            // `emit_map_val_user_drop_bodies_fn`.
            if let Value::EnumVariant { enum_name, .. } = &v {
                if enum_name == "Option" || enum_name == "Result" {
                    self.run_discarded_value_user_drops(v);
                    continue;
                }
            }
            // B-2026-08-03-7 — a TUPLE-valued V (`Map[i64, (Option[Res], i64)]`).
            // A tuple TE has no declared HEAD, so the name gate below can never
            // admit it; codegen's twin is the tuple arm of
            // `emit_map_val_user_drop_bodies_fn`, which hands the per-value blob
            // to the same element walker.
            if let Value::Tuple(items) = &v {
                let items = items.clone();
                self.run_tuple_item_user_drops(items);
                continue;
            }
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
        // B-2026-08-28-58 — a payload that is a user ENUM, not just a struct.
        // The `Value::Struct` bind below rejected it outright, which is why
        // `let o: Option[E] = Some(E.B);` ran no body on ANY backend. Same
        // declared-head-vs-runtime-name gate as the struct arm, then the
        // enum's own body followed by its live variant's payload walk — the
        // order the compiled twin emits and the order a direct
        // `let x = E.A(R { .. });` already prints in.
        if let Value::EnumVariant {
            enum_name: pn,
            variant: pv,
            ..
        } = payload
        {
            if pn == "Option" || pn == "Result" {
                return;
            }
            if declared_head.as_deref() != Some(pn.as_str()) {
                return;
            }
            let pn = pn.clone();
            let _ = pv;
            let payload = payload.clone();
            if self.program.drop_method_keys.contains_key(&pn) {
                self.run_user_drop_body_only(&pn, payload.clone());
            }
            self.run_enum_payload_user_drops_value(&payload);
            return;
        }
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
                // B-2026-08-01-27 — a `let`-move of a Vec binding
                // (`let mut v = w;`) must not leave the new binding ALIASING
                // the moved-from source's storage. `Value::Array` is an
                // `Arc<RwLock<..>>`, so the plain clone taken by RHS eval
                // shares storage: a later `v.push(x)` was visible through
                // `w` (and through a closure's captured env slot across
                // calls — the `let mut v = outer; v.push(x)` body shape read
                // 3 where both compiled backends read 2). Codegen bit-copies
                // the header on a move, so post-move observations of the
                // source see the frozen pre-move value; bind a deep clone
                // here to match. Reference-semantics values inside the
                // elements keep their identity (`deep_clone_value` Arc-bumps
                // SharedStruct/Sender/Receiver/SharedCell/Atomic — the same
                // tool the callee-entry param copies use). Slices are
                // excluded: a slice rebind copies the VIEW (both windows
                // alias one buffer) on every backend, and materializing an
                // owned Array here would change its shape.
                // B-2026-08-13-16 widens that rule on BOTH of its axes, because
                // the aliasing it describes was never specific to `Value::Array`
                // or to an identifier RHS — those were just the two coordinates
                // the reported shape happened to sit at. `Value::Struct` holds
                // its fields by value, so a struct whose field is a `Vec` copies
                // the HashMap and Arc-BUMPS the field: `let mut t = a;
                // t.lines.push(x)` was visible through `a`. Measured across the
                // whole grid, interpreter vs AOT, with the compiled backend on
                // the design-correct answer every time (design.md classes
                // `let y = v` as a CONSUME, so the reuse must read an
                // independent copy):
                //
                //   struct <- identifier     interp 2 2   build 2 1
                //   Vec    <- struct field   interp 2 2   build 2 1
                //   struct <- struct field   interp 2 2   build 2 1
                //   struct <- Vec index      interp 2 2   build 2 1
                //   Map[K, Vec] <- ident     interp 2 2   build 2 1
                //
                // So: any PLACE-rooted RHS (the value keeps living where it was
                // read from), any value kind `deep_clone_value` materializes.
                // Everything else is untouched by construction — a non-place RHS
                // is a fresh temp with nothing to alias, and for every value
                // kind `deep_clone_value` does not handle it falls through to
                // the same `.clone()` this code already took.
                //
                // The two exclusions are deliberate and both predate this. A
                // SLICE is a VIEW: rebinding one copies the window on every
                // backend, and `deep_clone_value` would materialize an owned
                // Array instead, changing its shape — so it stays out, as the
                // note above says. Reference-semantics values (`shared struct`,
                // channel ends, `SharedCell`, `Atomic`) are shared BY DESIGN and
                // keep aliasing for free: `deep_clone_value` Arc-bumps them.
                // B-2026-08-14-2 — an int RHS at a FLOAT-annotated binding is an
                // implicit widening the language permits and the interpreter
                // did not perform, so the slot kept an Int. Not a cosmetic
                // divergence: `let x: f64 = some_u8; x == 200.0` ABORTED here
                // (no mixed Int/Float operator arm) on a program `karac check`
                // passes and `karac build` runs correctly.
                // B-2026-08-30-34 — the RHS's unsigned width, so a `u64`
                // widening into a float-annotated slot converts the value
                // rather than its two's-complement carrier.
                let src_u = self.span_unsigned_int_width(&value.span);
                // B-2026-08-30-48 half (a) — an aggregate LITERAL's own type is
                // not an integer, so `src_u` above is `None` for it and every
                // element converted as signed: `let t: (f64, i64) = (u, 1)` with
                // `u: u64 = u64::MAX` read -1 against both compiled backends'
                // 1.8446744073709552e19. Each element has its own expression and
                // its own signedness, so resolve them here where the spans are.
                //
                // A CONSTRUCTOR CALL is the same shape one level in: `Some(u)`
                // has no integer type of its own either, so the payload also
                // converted as signed — `let o: Option[f64] = Some(u)` read -1
                // where `Some(i)` at the same width read correctly. The args
                // are positional here and positional in `EnumData::Tuple`, so
                // the same index carries them across.
                let elem_widths: Vec<Option<u32>> = match &value.kind {
                    ExprKind::Tuple(elems) | ExprKind::ArrayLiteral(elems) => elems
                        .iter()
                        .map(|e| self.span_unsigned_int_width(&e.span))
                        .collect(),
                    ExprKind::Call { args, .. } => args
                        .iter()
                        .map(|a| self.span_unsigned_int_width(&a.value.span))
                        .collect(),
                    _ => Vec::new(),
                };
                let val = match ty.as_ref() {
                    Some(te) => super::exec::coerce_int_value_to_declared_float_elems(
                        val,
                        te,
                        src_u,
                        &elem_widths,
                    ),
                    None => val,
                };
                let rhs_is_place = matches!(
                    &value.kind,
                    ExprKind::Identifier(_)
                        | ExprKind::FieldAccess { .. }
                        | ExprKind::Index { .. }
                        | ExprKind::TupleIndex { .. }
                );
                let val = if matches!(&pattern.kind, crate::ast::PatternKind::Binding(_))
                    && rhs_is_place
                    && !matches!(&val, Value::Slice { .. })
                {
                    super::exec::deep_clone_value(&val)
                } else {
                    val
                };
                // Whole-value container moves out of the RHS (`let b = a;`,
                // `Box2 { s: d }`, `(h, 1)`): silence the sources' walks
                // before binding, then re-arm the freshly-bound names so a
                // stale record from a prior same-named binding can't silence
                // the new one.
                self.record_container_bodies_move_sources(value);
                // B-2026-08-03-3 — `let x = t.N` moves ONE tuple element out.
                // The destination now owns its body; without this record the
                // source tuple's element walk fired a full duplicate at scope
                // exit (codegen's twin `disarm_tuple_elem_bodies_at` re-emits
                // the walker with the same index masked). Recorded BEFORE the
                // re-arm loop below, which only clears names being (re)bound —
                // `t` is not one of them.
                if let ExprKind::TupleIndex { object, index } = &value.kind {
                    if let ExprKind::Identifier(src) = &object.kind {
                        self.moved_out_tuple_elem_bodies
                            .insert((src.clone(), *index as usize));
                    }
                }
                // B-2026-08-03-8 — the struct-FIELD peer: `let x = h.f` moves
                // one field out, so the source's field walk must skip it.
                // B-2026-09-03-11 — and a DEEPER chain (`let (r, k) = g.h.pe;`)
                // records the whole path instead. The identifier-object test
                // below is what limited this to one hop: `g.h` is a
                // `FieldAccess`, so nothing was recorded and the root's walk
                // kept the leaf's body.
                // B-2026-09-04-7 — but NOT for a `Copy` scalar leaf. A `Copy`
                // field read takes nothing out: the typechecker says so where
                // it exempts a `Copy` field from `partial_move_of_drop_struct`
                // ("a `Copy` field is read, not moved: the struct keeps every
                // field and the drop body still sees them all"), so a mask
                // recorded for one is pure damage.
                //
                // At ONE hop the damage was invisible — masking a body-less
                // field only removes a walk step that does nothing, which is
                // why `let z = h.n;` was always correct. At TWO the mask is a
                // PATH, and `remove_field_at_path` deletes the leaf out of the
                // intermediate STRUCT: `let z = h.a.id;` over
                // `struct H { a: R, b: R }` left `h.a` as an `R` with no `id`,
                // and `R`'s own drop body then read `self.id` off it and hit
                // the `unreachable!` in `read_field` — an ICE, on a program the
                // typechecker is right to accept.
                //
                // NARROW ON PURPOSE, and the asymmetry is the reason. Skipping
                // a mask that WAS needed doubles a `Drop` body; keeping one
                // that was not is inert unless its leaf belongs to a
                // Drop-bearing struct, which is the bug above. So the test is
                // not "does this value carry Drop work" but the far smaller
                // "is this leaf definitely `Copy`" — a scalar owns nothing, so
                // declining the mask for one cannot lose an owner. The broad
                // form was tried and is wrong: `field_value_carries_user_drop`
                // answers a `Tuple` by asking `value_runs_user_drop` of each
                // element, which is struct-only, so `let (k, _) = h.pe;` over
                // `(i64, Option[R])` read as "no Drop work", skipped a mask it
                // needed and ran `dR3` twice
                // (`test_destructure_owner_mask_reaches_the_remaining_arms`).
                //
                // Depth 1 is left ALONE for the same asymmetry: it is measured
                // correct, and re-gating it buys nothing this bug needs.
                //
                // `shared` joins the scalars because it is the typechecker's
                // OTHER exemption from the same rule — `copy_is_only_an_rc_retain`,
                // "a `shared` read RETAINS" — and so reaches the identical ICE
                // by the identical route: `let s = h.a.sh;` over a
                // Drop-bearing `R2 { id, sh }` deleted `sh` out of `h.a` and
                // `R2`'s body then read `self.sh.v` off what was left. A retain
                // leaves the source holding its own handle, so declining the
                // mask cannot orphan an owner — and codegen already declines
                // it, since `type_runs_user_drop` answers `false` for a shared
                // type, which is why the three compiled surfaces are the oracle
                // this arm is being matched to.
                if let ExprKind::FieldAccess { object, field } = &value.kind {
                    if let ExprKind::Identifier(src) = &object.kind {
                        self.moved_out_struct_field_bodies
                            .insert((src.clone(), field.clone()));
                    } else if !matches!(
                        &val,
                        Value::Int(_)
                            | Value::Float(_)
                            | Value::Bool(_)
                            | Value::Char(_)
                            | Value::SharedStruct(_)
                    ) {
                        if let Some((root, path)) = Self::field_chain_name_path(value) {
                            self.moved_out_nested_field_bodies.insert((root, path));
                        }
                    }
                    let _ = field;
                }
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
                    // B-2026-08-31-35 — this site now owns the value, so the
                    // taken arm's consumed locals must not run a second body.
                    self.disarm_discarded_tail_sources(value);
                    // B-2026-08-28-39 — and its PAYLOAD, for an enum that
                    // declares its own `impl Drop`. The walk above runs such an
                    // enum's OWN body and deliberately stops there, on the
                    // stated ground that firing payloads "would print bodies
                    // `karac build` does not" (the B-2026-08-01-2 p3 probe).
                    // That premise no longer holds AT THIS SITE: both compiled
                    // backends now print `drop E` AND the payload's `drop R41`
                    // for `let _ = E.A(R { .. })`, so the rule was preserving a
                    // parity that had already moved. Measured 1 / 2 / 2 before
                    // this line, and the compiled pair agrees with what a BOUND
                    // local of the same type does on every backend, which is
                    // what says the interpreter is the wrong side.
                    //
                    // Added HERE rather than inside `run_discarded_value_user_-
                    // drops`, and that placement is the whole care in this fix:
                    // that walker has 31 callers, one of which is the wildcard
                    // LEAF path (B-2026-08-28-12), where compiled runs the own
                    // body ALONE. Widening the shared walker would have fixed
                    // this site and simultaneously pushed the leaf from one body
                    // to two against compiled's one — trading a divergence for a
                    // divergence. The leaf's own missing payload body is a
                    // separate, backend-CONSISTENT gap tracked as
                    // B-2026-08-28-40.
                    // GATED ON AN INLINE CONSTRUCTION, and that gate is
                    // measured. Compiled runs the payload body for a variant
                    // built AT the discard (`let _ = E.A(R { .. })`) but not for
                    // one that arrives from a call (`let _ = mk()`, where both
                    // backends run the own body alone). Firing unconditionally
                    // here fixed the first and broke the second — 1/1/1 became
                    // 2/1/1 — so this matches the spelling compiled actually
                    // walks, and the call spelling stays where both backends
                    // already agree.
                    //
                    // B-2026-08-29-30 — asked of the DISCARD PRODUCER, not of
                    // the RHS node, so a construction reached through a
                    // no-`else` `if` (`let _ = if n == 1 { E.A(R { .. }) };`)
                    // answers the same as the direct spelling. Without the
                    // peel this site ran the enum's own body alone while both
                    // compiled backends ran the payload's too — the divergence
                    // this row's fix would otherwise have introduced, in the
                    // one shape it newly admits.
                    //
                    // ONLY the no-`else` `if` is peeled, for the reason
                    // B-2026-08-29-30 gave: a two-tail `if` or a `match`
                    // yields its value to the STATEMENT site on the compiled
                    // side, which registered no payload walk for an enum ctor
                    // arm — peeling those would have moved this backend from
                    // agreeing at one body to disagreeing at two.
                    // B-2026-08-31-22 — the two-tail `if` and the `match` are
                    // peeled TOO, now that the premise above no longer holds.
                    // The statement discard site on the compiled side used to
                    // register nothing for a branch of enum ctors, so peeling
                    // here would have traded agreement at one body for
                    // disagreement at two; it registers the wrapper AND the
                    // payload walk as of this commit, so peeling is what keeps
                    // the two backends together instead of what splits them.
                    // B-2026-08-31-21 — the `shared` leg, asked before the
                    // clone below so the last-reference test sees the live
                    // count. A `shared` literal discarded here holds the only
                    // reference; one that came off a still-live binding
                    // (`let _ = a;`) does not, and keeps its own path.
                    self.run_discarded_shared_user_drop(&val);
                    // B-2026-09-02-13 — …or a METHOD producer returning an
                    // owned enum by declared return. The static gate has no
                    // `MethodCall` arm and cannot grow one usefully: deciding it
                    // needs the RECEIVER's type, which only the evaluated value
                    // carries here. Asked value-side instead, with the identical
                    // test `discard_rhs_produces_owned_value` already uses for
                    // this shape — the builtin borrow names excluded outright,
                    // then a user `impl` method whose declared return names this
                    // type. Codegen's registrar reaches `f.make()` through its
                    // own MethodCall arm and now registers the payload walker
                    // beside the wrapper, so declining here would be the
                    // divergence.
                    let method_producer = matches!(&value.kind, ExprKind::MethodCall { method, .. }
                        if !matches!(method.as_str(), "get" | "first" | "last" | "peek")
                            && matches!(&val, Value::EnumVariant { enum_name, .. }
                                if self.user_method_returns_owned_type(method, enum_name)));
                    let inline_ctor =
                        method_producer || self.discard_taken_producer_runs_payload_walk(value);
                    if inline_ctor {
                        if let Value::EnumVariant { enum_name, .. } = &val {
                            if self.program.drop_method_keys.contains_key(enum_name) {
                                let v = val.clone();
                                self.run_enum_payload_user_drops_value(&v);
                            }
                        }
                    }
                }
                // B-2026-08-28-12 — the same discard, one level IN. A wildcard
                // LEAF of a tuple or struct pattern (`let (_, n) = p;`,
                // `let W { r: _, n } = w;`) binds nothing, so
                // `push_drops_for_stmt` registers no slot for it and the
                // element's user `Drop` body ran ZERO times — against
                // design.md § Drop's "dropped exactly once at the live-range
                // end of its final owner". The whole-pattern arm above has
                // covered `let _ = <owned>;` since B-2026-07-30-11 and a
                // `match` arm's wildcard is correct too, so a wildcard leaf
                // inside a `let` pattern was the one position with no owner
                // anywhere. Fire at the destructure point, which is where the
                // discarded element dies and where both compiled backends
                // already fire the struct spelling.
                self.run_wildcard_destructure_leaf_user_drops(pattern, value, &val);
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
                    // B-2026-09-03-15 — tuple leg. Recording the ELEMENT types
                    // here is what lets a later `let (r, o) = t;` resolve its
                    // leaves at all; the destructure's source is a bare name,
                    // which the span-keyed chain above cannot say anything
                    // about.
                    self.record_tuple_var_elem_tes(&bname, ty, value);
                }
                // B-2026-09-03-15 — the DESTRUCTURE's own registration moment.
                // Everything above is gated on a whole-value `Binding`, so a
                // tuple pattern's `Option`/`Result` leaves reached no
                // registration and their payload bodies ran nowhere.
                self.record_destructure_optres_payload_tes(pattern, ty, value);
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
                // (both edges), mirroring codegen. B-2026-08-30-17 corrected
                // the miss-edge ORDER: it used to fire AFTER the else block,
                // described here as "matching codegen's drop-during-return
                // order" — which was true of codegen and wrong against
                // design.md, so both backends moved together.
                let scrut_drop = self.freshtemp_scrutinee_user_drop_type(value);
                let drop_val = scrut_drop.as_ref().map(|_| val.clone());
                // B-2026-07-31-45 — the `let…else` twin of the match/if-let
                // disarm: this pattern moves a Drop-bearing payload out into
                // an ESCAPING binding, so the source's payload-body walk must
                // skip it or the body runs twice (once via the source's walk
                // at its NLL death — before the binding is even used — and
                // once via the binding's own slot).
                // `None`: a `let … else` pattern binds into the ENCLOSING
                // block, so B-2026-08-28-67's read-through gate has no scope to
                // inspect and the payload is materialized by definition.
                //
                // B-2026-09-02-14 — INSIDE the match test since this row, like
                // its `if let` / `while let` siblings. It used to run ahead of
                // the test because codegen's retraction was a compile-time
                // removal; codegen now clears a per-path flag on the match edge
                // instead, so the else edge keeps the walk it never gave away.
                if self.try_match_pattern(pattern, &val) {
                    self.disarm_moved_out_enum_payload_one(value, &val, pattern, None);
                    // B-2026-08-31-30 — the STRUCT sibling of the enum-payload
                    // disarm above, and the same failure it describes: a bare
                    // struct pattern (`let P { r, .. } = h else { … }`) moves
                    // each bound field into an ESCAPING binding, but the
                    // source's field-bodies walk still visited them, so the
                    // body ran TWICE — once at `h`'s NLL death, BEFORE the
                    // binding is even used, and once via the binding's own
                    // slot. Measured `dR[a] a dR[a] end` against every compiled
                    // backend's `a dR[a] end`.
                    //
                    // Masks per FIELD rather than retracting the source
                    // wholesale, so a field the pattern leaves unbound still
                    // gets its body. This is the same cut codegen's
                    // `disarm_arm_destructured_struct_field_bodies` makes, and
                    // `moved_out_struct_field_bodies` is the map
                    // `drop_user_drop_fields_of_binding` already consults
                    // (B-2026-08-03-8) — the `let x = h.f` spelling has used it
                    // since then; only this one never registered.
                    //
                    // Match edge only: the `else` edge binds nothing, so the
                    // source keeps every body it owes.
                    if let (
                        crate::ast::PatternKind::Struct { fields, .. },
                        ExprKind::Identifier(src),
                    ) = (&pattern.kind, &value.kind)
                    {
                        for fp in fields {
                            let whole_move = match &fp.pattern {
                                None => true,
                                Some(p) => {
                                    matches!(p.kind, crate::ast::PatternKind::Binding(_))
                                }
                            };
                            if whole_move {
                                self.moved_out_struct_field_bodies
                                    .insert((src.clone(), fp.name.clone()));
                            }
                        }
                    }
                    self.bind_pattern(pattern, val);
                    if let (Some(tn), Some(dv)) = (scrut_drop, drop_val) {
                        self.run_user_drop_body_on_value(&tn, dv);
                    }
                } else {
                    // B-2026-08-30-17 — BEFORE the else block, not after it.
                    // design.md § "Scrutinee temporary scope", third bullet:
                    // "In the **`else` block of `let...else`**: same rule —
                    // scrutinee temporaries are dropped before the divergent
                    // else block runs." The worked example in that section is
                    // explicit that the lease is "already released — the error
                    // path runs without it held".
                    if let (Some(tn), Some(dv)) = (scrut_drop, drop_val) {
                        self.run_user_drop_body_on_value(&tn, dv);
                    }
                    // B-2026-09-02-12 — and the payload bodies of a FRESH-TEMP
                    // `Option`/`Result` the pattern declined, on the same edge
                    // and for the same reason as the `if let` / `while let`
                    // sites: the declared-type walk skips `Ok(T)` / `Err(E)`,
                    // whose payloads are the enum's own generic parameters, so
                    // `let Ok(w) = mkerr() else { … }` ran `W`'s body on no
                    // surface while `mkerr();` ran it on every one. Fresh temps
                    // only — a named local reaching this edge still has its own
                    // walk, and firing here as well would double it.
                    if Self::optres_freshtemp_scrutinee(value)
                        && self.scrutinee_expr_is_consuming(value)
                    {
                        self.run_optres_payload_user_drops_value(&val);
                    }
                    let else_result = self.eval_block_inner(else_block);
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
                // B-2026-08-14-6 — an INT RHS landing in a FLOAT slot
                // (`v[i] = some_u8` on a `Vec[f64]`, and the field / plain-binding
                // spellings alike). Codegen converts at the store; the interpreter
                // kept the `Int`, so the container's contents disagreed with its
                // declared element type and a later `contains(200.0)` answered false.
                // Keyed by the RHS's span, which the typechecker flagged when it
                // checked the value against the target's type.
                let val = self.coerce_float_assign_rhs(value, val);
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
                                // B-2026-08-03-2 (class 1) — the CONTAINER
                                // sibling of the two arms above: `v = w` over a
                                // `Vec[Res]` displaced every old element and ran
                                // none of their bodies, because this match only
                                // ever saw a Struct or an EnumVariant. Codegen
                                // had the mirror-image gap — its reassign path
                                // released the old elements' memory and freed
                                // the buffer with no body walk. Skipped when the
                                // binding already moved its value out, matching
                                // the guard the other arms use.
                                Value::Array(ref rc)
                                    if !self
                                        .moved_out_container_bodies_bindings
                                        .contains(t.as_str()) =>
                                {
                                    let elems: Vec<Value> =
                                        rc.read().map(|g| g.clone()).unwrap_or_default();
                                    for e in elems {
                                        self.run_discarded_value_user_drops(e);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                // B-2026-08-01-20 — FIELD-target sibling of the leg above:
                // `o.h = <new>;` displaces the old field value, whose Drop
                // work was silently discarded (the memory side has its own
                // machinery; bodies never ran on either backend). Same
                // guards, value-based fire (a field has no binding name for
                // the disarm sets — the BASE-coarse move record stands in,
                // matching the codegen twin's armed-action gate).
                if let ExprKind::FieldAccess { object, field } = &target.kind {
                    // B-2026-08-01-30 leg A — flatten a DEEP chain base
                    // (`o.h.r = <new>`): collect the middle field names down
                    // to the root identifier, then walk the root's Struct
                    // value through them. Depth 1 keeps the original
                    // behavior; a non-Identifier chain link declines.
                    let mut chain_middles: Vec<String> = Vec::new();
                    let mut chain_cur = object;
                    let chain_base = loop {
                        match &chain_cur.kind {
                            ExprKind::Identifier(root) => break Some(root.clone()),
                            ExprKind::FieldAccess {
                                object: inner,
                                field: mid,
                            } => {
                                chain_middles.push(mid.clone());
                                chain_cur = inner;
                            }
                            _ => break None,
                        }
                    };
                    if let Some(base) = chain_base {
                        chain_middles.reverse();
                        // B-2026-08-30-54 — the PER-FIELD gate beside the
                        // three base-coarse ones. `h.f = r` over a param view
                        // records `(h, f)` below, and a LATER `h.f = <fresh>`
                        // displaces that view -- a value the caller still owns
                        // and fires. Without this the interpreter ran it here
                        // as well: `dR0 m1 dR8 m2 dR5 dE dR8 v5`, one `dR8` too
                        // many, against the compiled backends' (and the
                        // bare-identifier spelling's) `dR0 m1 m2 dR5 dE dR8 v5`.
                        //
                        // Depth 1 only, because that is the key shape the set
                        // has: a deep chain (`o.h.r = ..`) is keyed by the ROOT
                        // and its own field name, which is not what a middle
                        // segment's view would record.
                        let field_is_view = chain_middles.is_empty()
                            && self
                                .moved_out_struct_field_bodies
                                .contains(&(base.clone(), field.clone()));
                        if !field_is_view
                            && !self.moved_out_user_drop_bindings.contains(base.as_str())
                            && !self.moved_out_drop_field_bindings.contains(base.as_str())
                            && !crate::deque_head::expr_mentions_name_deep(value, &base)
                        {
                            let mut parent = self.env.get(&base);
                            for mid in &chain_middles {
                                parent = match parent {
                                    Some(Value::Struct { fields, .. }) => fields
                                        .iter()
                                        .find(|(n, _)| n.as_str() == mid.as_str())
                                        .map(|(_, v)| v.clone()),
                                    _ => None,
                                };
                            }
                            let old_field = match parent {
                                Some(Value::Struct { fields, .. }) => fields
                                    .iter()
                                    .find(|(n, _)| n.as_str() == field.as_str())
                                    .map(|(_, v)| v.clone()),
                                _ => None,
                            };
                            if let Some(old) = old_field {
                                match &old {
                                    Value::Struct { name: tn, .. } => {
                                        if self.program.drop_method_keys.contains_key(tn) {
                                            let tn = tn.clone();
                                            self.run_user_drop_body_on_value(&tn, old);
                                        } else if self.value_runs_user_drop(&old) {
                                            self.drop_user_drop_fields_of_value(&old);
                                        }
                                    }
                                    Value::EnumVariant { enum_name, .. }
                                        if enum_name != "Option" && enum_name != "Result" =>
                                    {
                                        if self.program.drop_method_keys.contains_key(enum_name) {
                                            let tn = enum_name.clone();
                                            self.run_user_drop_body_on_value(&tn, old.clone());
                                        }
                                        self.run_enum_payload_user_drops_value(&old);
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                // B-2026-08-01-21 — INDEX-target sibling: `v[i] = <new>`
                // displaces the old element, whose Drop bodies were silent
                // (the memory side is codegen-only — interp values are
                // GC'd). Simple index shapes only, mirroring the codegen
                // twin's effect-free re-evaluation rule. B-2026-08-01-22
                // leg a: field-rooted containers (`h.xs[i] = <new>`) take
                // the same fire, reading the array out of the base
                // binding's field.
                if let ExprKind::Index { object, index } = &target.kind {
                    let field_rooted = match &object.kind {
                        ExprKind::FieldAccess {
                            object: inner,
                            field,
                        } => match &inner.kind {
                            ExprKind::Identifier(base) => Some((base, field)),
                            _ => None,
                        },
                        _ => None,
                    };
                    if let Some((base, fname)) = field_rooted {
                        let simple_index = Self::assign_index_is_pure_scalar(index);
                        if simple_index
                            && !self.moved_out_user_drop_bindings.contains(base.as_str())
                            && !self.moved_out_drop_field_bindings.contains(base.as_str())
                            && !crate::deque_head::expr_mentions_name_deep(value, base)
                        {
                            let idx = match self.eval_expr_inner(index) {
                                Value::Int(i) if i >= 0 => Some(i as usize),
                                _ => None,
                            };
                            let arr = match self.env.get(base) {
                                Some(Value::Struct { fields, .. }) => fields
                                    .iter()
                                    .find(|(n, _)| n.as_str() == fname.as_str())
                                    .map(|(_, v)| v.clone()),
                                _ => None,
                            };
                            let old_elem = match (idx, arr) {
                                (Some(i), Some(Value::Array(rc))) => {
                                    let guard = rc.read().unwrap();
                                    guard.get(i).cloned()
                                }
                                _ => None,
                            };
                            if let Some(old) = old_elem {
                                match &old {
                                    Value::Struct { name: tn, .. } => {
                                        if self.program.drop_method_keys.contains_key(tn) {
                                            let tn = tn.clone();
                                            self.run_user_drop_body_on_value(&tn, old);
                                        } else if self.value_runs_user_drop(&old) {
                                            self.drop_user_drop_fields_of_value(&old);
                                        }
                                    }
                                    Value::EnumVariant { enum_name, .. }
                                        if enum_name != "Option" && enum_name != "Result" =>
                                    {
                                        if self.program.drop_method_keys.contains_key(enum_name) {
                                            let tn = enum_name.clone();
                                            self.run_user_drop_body_on_value(&tn, old.clone());
                                        }
                                        self.run_enum_payload_user_drops_value(&old);
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    if let ExprKind::Identifier(vname) = &object.kind {
                        let simple_index = Self::assign_index_is_pure_scalar(index);
                        if simple_index
                            && !self.moved_out_user_drop_bindings.contains(vname.as_str())
                            && !crate::deque_head::expr_mentions_name_deep(value, vname)
                        {
                            let idx = match self.eval_expr_inner(index) {
                                Value::Int(i) if i >= 0 => Some(i as usize),
                                _ => None,
                            };
                            let old_elem = match (idx, self.env.get(vname)) {
                                (Some(i), Some(Value::Array(rc))) => {
                                    let guard = rc.read().unwrap();
                                    guard.get(i).cloned()
                                }
                                _ => None,
                            };
                            if let Some(old) = old_elem {
                                match &old {
                                    Value::Struct { name: tn, .. } => {
                                        if self.program.drop_method_keys.contains_key(tn) {
                                            let tn = tn.clone();
                                            self.run_user_drop_body_on_value(&tn, old);
                                        } else if self.value_runs_user_drop(&old) {
                                            self.drop_user_drop_fields_of_value(&old);
                                        }
                                    }
                                    Value::EnumVariant { enum_name, .. }
                                        if enum_name != "Option" && enum_name != "Result" =>
                                    {
                                        if self.program.drop_method_keys.contains_key(enum_name) {
                                            let tn = enum_name.clone();
                                            self.run_user_drop_body_on_value(&tn, old.clone());
                                        }
                                        self.run_enum_payload_user_drops_value(&old);
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                // B-2026-08-30-34 — the RHS's own unsigned width, so
                // `s.f = some_u64` into an `f64` field converts the value.
                let src_u = self.span_unsigned_int_width(&value.span);
                if !self.assign_to_place(target, val, src_u) {
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
                    // ORDER MATTERS: the re-arm above CLEARS this very set, so
                    // the view record has to be taken afterwards or it is wiped
                    // by the statement that establishes it.
                    self.record_assign_of_param_view(&t, value);
                } else if let ExprKind::FieldAccess { object, field } = &target.kind {
                    // B-2026-08-30-54 — the FIELD sibling of the two calls
                    // above, expressed over `moved_out_struct_field_bodies`
                    // rather than the whole-binding set, which is what the row
                    // asked for and what keeps the mask per field.
                    //
                    // Same order and same reason: clear first (the field just
                    // received a value, so a stale record from its previous one
                    // must not silence it), then record if what it received is
                    // a param view.
                    if let ExprKind::Identifier(base) = &object.kind {
                        let (base, field) = (base.clone(), field.clone());
                        self.moved_out_struct_field_bodies
                            .remove(&(base.clone(), field.clone()));
                        // Keyed by a field PATH, so a prefix match rather
                        // than a point removal: every payload record rooted at
                        // this field is stale once the field is overwritten.
                        self.moved_out_struct_field_payload_bodies
                            .retain(|(n, path)| !(n == &base && path.first() == Some(&field)));
                        self.record_field_assign_of_param_view(&base, &field, value);
                    }
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
                let unsigned_hint = self.span_unsigned_int_width(&target.span);
                let result = self.eval_binary(&bin_op, current, rhs, &stmt.span, unsigned_hint);
                // Route through `assign_to_place` so compound assignment works
                // on field / index / nested targets (`o.count += 1`,
                // `v[i].x += 1`), not just bare bindings. Previously only the
                // `Identifier` target was handled — field/index compound
                // assigns were silently dropped.
                if !self.assign_to_place(target, result, None) {
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
                //
                // B-2026-08-29-25 — dispatch on the block-WRAPPED tail, not on
                // the wrapper. Codegen's discard gates have peeled `{ … }` /
                // `unsafe { … }` / a labeled block down to their tail since
                // slice 5; this dispatch never did, so `{ mk(7) };` and
                // `{ match n { … } };` were silent on this backend while all
                // three compiled surfaces fired. See
                // `discard_stmt_shape_expr` for the one shape a wrapper is
                // not peeled for.
                let shape = self.discard_stmt_shape_expr(expr);
                match &shape.kind {
                    ExprKind::Call { callee, .. } => {
                        // Bare Path-callee CTOR discard (`Option.Some(mk());`,
                        // `Sig.A(g);`): the wildcard-let gate admits Path
                        // ctors unconditionally, and codegen's bare arm now
                        // registers the same optres/enum payload bodies —
                        // route through the shared discard walker so both
                        // statement shapes agree (B-2026-07-30-11, optres
                        // bare leg).
                        if matches!(&callee.kind, ExprKind::Path { .. }) {
                            // B-2026-08-28-48 — and its PAYLOAD, for an enum
                            // that declares its own `impl Drop`. The shared
                            // walker runs such an enum's OWN body and stops, so
                            // `E.A(R { id: 41 });` was 1 body here against
                            // compiled's 2 — while the `let _ =` spelling of the
                            // same construction is 2 on every backend, and a
                            // BOUND local is too.
                            //
                            // SITE-LOCAL, and that placement is the whole care
                            // in this fix, for the reason B-2026-08-28-39
                            // recorded one statement kind over and
                            // B-2026-08-28-40 re-applied at the wildcard leaf:
                            // `run_discarded_value_user_drops` has ~31 callers,
                            // among them the CALL-source discard that compiled
                            // deliberately runs at ONE body (the `Identifier`
                            // callee arm just below). Widening the shared walker
                            // would fix this site and break that one — trading a
                            // divergence for a divergence.
                            //
                            // No inline-ctor gate is needed here, unlike the
                            // `let _ =` twin: a `Path` callee in statement
                            // position IS the inline construction, and a value
                            // arriving from a call takes the `Identifier` arm.
                            // B-2026-08-31-21 — before the clone, which would
                            // bump the `Arc` and defeat the walker's
                            // last-reference test; see
                            // `run_discarded_shared_user_drop`. Inert for the
                            // enum shapes these arms are otherwise about.
                            self.run_discarded_shared_user_drop(&discarded);
                            let payload_src = discarded.clone();
                            self.run_discarded_value_user_drops(discarded);
                            if let Value::EnumVariant { enum_name, .. } = &payload_src {
                                if self.program.drop_method_keys.contains_key(enum_name) {
                                    self.run_enum_payload_user_drops_value(&payload_src);
                                }
                            }
                        } else if let ExprKind::Identifier(fn_name) = &callee.kind {
                            // B-2026-08-28-48 (bare-unqualified leg) — `A(R { .. });`
                            // is the SAME construction as the `Path` arm above,
                            // spelled without its enum. It ran NOTHING here — 0
                            // against compiled's 2 — because the gate below looks
                            // for a program FUNCTION and a variant name is not
                            // one, so the discard was declined outright rather
                            // than merely losing the payload.
                            //
                            // Exactly the mechanism B-2026-08-28-39 recorded for
                            // the `let _ =` spelling of this shape and fixed
                            // there; the statement position was never given the
                            // same treatment, so the two statement kinds
                            // disagreed for one spelling and not the other.
                            if self.find_enum_for_variant(fn_name).is_some() {
                                let payload_src = discarded.clone();
                                self.run_discarded_value_user_drops(discarded);
                                if let Value::EnumVariant { enum_name, .. } = &payload_src {
                                    if self.program.drop_method_keys.contains_key(enum_name) {
                                        self.run_enum_payload_user_drops_value(&payload_src);
                                    }
                                }
                            } else if let Some(tn) = self.user_fn_return_type_name(fn_name) {
                                if self.program.drop_method_keys.contains_key(&tn) {
                                    // B-2026-09-02-13 — the OWN body and, for an
                                    // enum, the live variant's PAYLOAD bodies.
                                    // The two are COMPLEMENTARY registrations for
                                    // an enum — `karac_drop_<E>`'s field-cleanup
                                    // half is a no-op for an enum name and cannot
                                    // reach a variant payload — and this arm made
                                    // only the first, so `mk(1);` printed `dB`
                                    // where `let x = mk(1);`, the identical value
                                    // one spelling away, prints `dB dW7`.
                                    //
                                    // Same shape and same order as the `Path`-ctor
                                    // arm above. SITE-LOCAL for the reason that
                                    // arm records: `run_discarded_value_user_drops`
                                    // has ~31 callers and widening it doubles the
                                    // body at the ones that already add their own
                                    // walk (measured: `let (_, _) = (mk(1), 5);`
                                    // went to `dB dW7 dW7`).
                                    let payload_src = discarded.clone();
                                    self.run_user_drop_body_on_value(&tn, discarded);
                                    if let Value::EnumVariant { .. } = &payload_src {
                                        self.run_enum_payload_user_drops_value(&payload_src);
                                    }
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
                    // B-2026-08-28-69 — a DISCARDED `match` whose arm value is
                    // an owned Drop-bearing temp. This dispatch had no `Match`
                    // arm, so the value never reached the shared discard walker
                    // and its body ran on this backend alone out of four.
                    //
                    // Landed TOGETHER with the codegen half (the `Match` arm of
                    // `try_track_discarded_user_drop_temp` and the struct-literal
                    // admission in `discarded_match_value_tail`), because either
                    // alone MOVES the divergence rather than removing it — the
                    // measurement that got an earlier interpreter-only attempt at
                    // this row reverted.
                    //
                    // Needs no gate for the shapes that must stay silent: the
                    // walker is value-driven, and a read-only arm yields `Unit`,
                    // for which it is a no-op.
                    ExprKind::Match { arms, .. } => {
                        // …but ONLY when no arm hands out a binding that is
                        // still LIVE here. That gate is not defensive — it was
                        // measured: `let r = R { id: 41 }; match n { 0 => r, _ =>
                        // R { id: 9 } };` yields an ENCLOSING local, which owns
                        // its own scope-exit body, and firing the walker on top
                        // printed `drop 41` TWICE
                        // (`test_conditionally_moved_local_user_drop_body_runs_once`,
                        // the `discarded-match-statement` row).
                        //
                        // Liveness is exactly the discriminator, and it is
                        // available for free at this point: an arm's payload
                        // BINDING has already left scope by the time the
                        // statement's discard runs, so it looks up to `None`
                        // and correctly fires; an enclosing local is still in
                        // scope, looks up to `Some`, and correctly does not.
                        // Shape alone cannot tell them apart — both are a bare
                        // `Identifier` at the arm tail.
                        //
                        // B-2026-09-01-11 — asked of the arm that RAN where one
                        // was recorded; see
                        // `discarded_branch_taken_tail_is_ownable`. The
                        // all-arms form below stays as the fallback, and is
                        // what still answers for a construct whose arm tail was
                        // never noted.
                        let hands_out_live_binding =
                            match self.discarded_branch_taken_tail_is_ownable(shape) {
                                Some(ownable) => !ownable,
                                None => arms.iter().any(|a| {
                                    matches!(&Self::arm_tail_expr(&a.body).kind,
                                    ExprKind::Identifier(n) if self.env.get(n).is_some())
                                }),
                            };
                        if !hands_out_live_binding {
                            // B-2026-08-31-21 — before the clone, which would
                            // bump the `Arc` and defeat the walker's
                            // last-reference test; see
                            // `run_discarded_shared_user_drop`. Inert for the
                            // enum shapes these arms are otherwise about.
                            self.run_discarded_shared_user_drop(&discarded);
                            let payload_src = discarded.clone();
                            self.run_discarded_value_user_drops(discarded);
                            // B-2026-08-31-35 — the BARE-STATEMENT spelling of
                            // the disarm at the wildcard-`let` gate. This site
                            // has just taken the value, so a local the taken
                            // arm's literal consumed must not run a second
                            // body. The `hands_out_live_binding` / `owns` gates
                            // above already decline an arm handing out a live
                            // binding WHOLE; a literal that CONSUMES one is the
                            // same move an aggregate deeper, and was invisible
                            // to both backends.
                            self.disarm_discarded_tail_sources(expr);
                            // B-2026-08-31-22 — and its PAYLOAD, exactly as the
                            // `Path`-ctor arm above runs one and for the same
                            // reason: the shared walker runs the enum's OWN body
                            // and stops, so a branch of ctors was 1 body here
                            // against compiled's 2. AFTER the own body, so the
                            // transcript order matches the direct spelling's
                            // `dE dR8` rather than inverting it.
                            //
                            // Gated on the same all-arms construction test the
                            // `let _ =` twin uses, so the two statement kinds
                            // admit one population — the drift B-2026-08-29-20
                            // had to repair once already on this trio.
                            if self.discard_taken_producer_runs_payload_walk(shape) {
                                if let Value::EnumVariant { enum_name, .. } = &payload_src {
                                    if self.program.drop_method_keys.contains_key(enum_name) {
                                        self.run_enum_payload_user_drops_value(&payload_src);
                                    }
                                }
                            }
                        }
                    }
                    // B-2026-08-29-25 — the `if` SPELLING of the arm above,
                    // silent here and on both compiled surfaces until this
                    // row. Same liveness gate, and it is needed for the same
                    // measured reason: `let r = R { id: 41 }; if n == 0 { r }
                    // else { R { id: 9 } };` hands out an ENCLOSING local that
                    // already owns its scope-exit body, and that shape is
                    // correct at one body today on all four surfaces —
                    // firing the walker on top would double it.
                    //
                    // A branch with no tail expression, and an `if` with no
                    // `else`, yield unit; the walker is value-driven and a
                    // no-op on `Unit`, so they need no gate of their own.
                    ExprKind::If {
                        then_block,
                        else_branch,
                        ..
                    } => {
                        // B-2026-08-29-30 (no-`else` half) — an `if` WITHOUT an
                        // `else` is admitted on its then-tail alone, and this
                        // arm's liveness gate is what makes that safe: a tail
                        // naming a live enclosing local is declined, so the
                        // only values admitted are ones the arm itself minted.
                        //
                        // It was declined outright until codegen could follow.
                        // `compile_if`'s merge yields a const-0 placeholder
                        // when there is no `else`, so the value the arm built
                        // never reached its discard SITE at all, and firing
                        // here alone made the shape a run-vs-build divergence
                        // rather than an agreed gap — the trade this file's
                        // `bare_removal` note refuses for `v.remove(i);`.
                        // Codegen now registers the owner INSIDE the arm
                        // (B-2026-08-29-5's second mechanism, widened past
                        // `Call`/`MethodCall` by
                        // `discarded_arm_owned_aggregate_tail`), so both
                        // backends fire together.
                        //
                        // B-2026-09-01-11 — the `match` arm's taken-tail gate,
                        // in the `if` spelling. The all-arms form is the
                        // fallback for a construct that recorded no arm tail,
                        // which is exactly the no-`else` shape the second arm
                        // below is written for.
                        let owns = self
                            .discarded_branch_taken_tail_is_ownable(shape)
                            .unwrap_or_else(|| {
                                match (then_block.final_expr.as_deref(), else_branch.as_deref()) {
                                    (Some(then_tail), Some(else_tail)) => [then_tail, else_tail]
                                        .into_iter()
                                        .map(Self::arm_tail_expr)
                                        .all(|tail| {
                                            !matches!(&tail.kind,
                                                ExprKind::Identifier(n) if self.env.get(n).is_some())
                                        }),
                                    (Some(then_tail), None) => {
                                        let tail = Self::arm_tail_expr(then_tail);
                                        !matches!(&tail.kind,
                                            ExprKind::Identifier(n) if self.env.get(n).is_some())
                                    }
                                    _ => false,
                                }
                            });
                        if owns {
                            // B-2026-08-31-21 — before the clone, which would
                            // bump the `Arc` and defeat the walker's
                            // last-reference test; see
                            // `run_discarded_shared_user_drop`. Inert for the
                            // enum shapes these arms are otherwise about.
                            self.run_discarded_shared_user_drop(&discarded);
                            let payload_src = discarded.clone();
                            self.run_discarded_value_user_drops(discarded);
                            // B-2026-08-31-35 — the BARE-STATEMENT spelling of
                            // the disarm at the wildcard-`let` gate. This site
                            // has just taken the value, so a local the taken
                            // arm's literal consumed must not run a second
                            // body. The `hands_out_live_binding` / `owns` gates
                            // above already decline an arm handing out a live
                            // binding WHOLE; a literal that CONSUMES one is the
                            // same move an aggregate deeper, and was invisible
                            // to both backends.
                            self.disarm_discarded_tail_sources(expr);
                            // B-2026-08-31-22 — and its PAYLOAD, exactly as the
                            // `Path`-ctor arm above runs one and for the same
                            // reason: the shared walker runs the enum's OWN body
                            // and stops, so a branch of ctors was 1 body here
                            // against compiled's 2. AFTER the own body, so the
                            // transcript order matches the direct spelling's
                            // `dE dR8` rather than inverting it.
                            //
                            // Gated on the same all-arms construction test the
                            // `let _ =` twin uses, so the two statement kinds
                            // admit one population — the drift B-2026-08-29-20
                            // had to repair once already on this trio.
                            if self.discard_taken_producer_runs_payload_walk(shape) {
                                if let Value::EnumVariant { enum_name, .. } = &payload_src {
                                    if self.program.drop_method_keys.contains_key(enum_name) {
                                        self.run_enum_payload_user_drops_value(&payload_src);
                                    }
                                }
                            }
                        }
                    }
                    ExprKind::MethodCall { method, .. }
                        if !matches!(method.as_str(), "get" | "first" | "last" | "peek") =>
                    {
                        // B-2026-08-03-2 (class 3) — a BUILTIN container
                        // removal discarded as a bare statement (`v.pop();`,
                        // `m.remove(k);`). The `let _ = v.pop();` form already
                        // fires, through the same method list in
                        // `discard_rhs_produces_owned_value`; this arm only
                        // ever admitted USER methods, so the two statement
                        // forms disagreed — and codegen fires for both, making
                        // the bare form a run-vs-build split in the
                        // interpreter-silent direction.
                        //
                        // Restricted to an OPTION/RESULT-shaped result, which
                        // is exactly codegen's current reach: its discard
                        // registrar is the optres payload walker, and the only
                        // receivers it resolves a type for are Map-shaped, so
                        // `v.pop()` and `m.remove(k)` (both `Option`-returning)
                        // are covered there and `v.remove(i)` / `v.swap_remove(i)`
                        // (bare `T`) are not. Admitting the bare-`T` forms here
                        // would FLIP them from both-silent to an
                        // interpreter-fires/AOT-silent split — measurably worse
                        // than the shared gap, since AOT additionally leaks
                        // them. Those stay class 2 on the row until the codegen
                        // side lands; this arm only removes a divergence.
                        let optres_result = matches!(
                            &discarded,
                            Value::EnumVariant { enum_name, .. }
                                if enum_name == "Option" || enum_name == "Result"
                        );
                        // B-2026-08-03-2 (class 2) — `v.remove(i);` /
                        // `v.swap_remove(i);` hand back the element BY VALUE.
                        // Held back on the first pass because codegen could not
                        // resolve a builtin removal's element type and so
                        // neither fired nor freed it; admitting them then would
                        // have turned a shared gap into a divergence. Codegen
                        // now resolves the element type from the receiver's
                        // recorded element TypeExpr, so both sides can own it.
                        let bare_removal = matches!(method.as_str(), "remove" | "swap_remove")
                            && matches!(
                                &discarded,
                                Value::Struct { .. } | Value::EnumVariant { .. }
                            );
                        if matches!(
                            method.as_str(),
                            "insert"
                                | "remove"
                                | "swap_remove"
                                | "pop"
                                | "pop_back"
                                | "pop_front"
                                | "take"
                        ) && (optres_result || bare_removal)
                        {
                            self.run_discarded_value_user_drops(discarded);
                            return Ok(Value::Unit);
                        }
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
                                        // B-2026-09-02-13 — the METHOD spelling
                                        // of the free-fn arm above, and the same
                                        // complementary pair: `f.make();` ran the
                                        // enum's own body alone where the bound
                                        // `let x = f.make();` runs `dB dW7`.
                                        // Codegen's registrar reaches this shape
                                        // through its MethodCall arm and now
                                        // registers both, so the walk here is
                                        // what keeps the two together.
                                        let payload_src = discarded.clone();
                                        self.run_user_drop_body_on_value(&tn, discarded);
                                        self.run_enum_payload_user_drops_value(&payload_src);
                                    } else if tn == "Option" || tn == "Result" {
                                        // B-2026-08-31-47 — `Option`/`Result`
                                        // take the SHARED discard walker, not
                                        // the declared-type payload walk beside
                                        // it. They are built-ins with no
                                        // source-level `EnumDef`, and that walk
                                        // is declared-type driven, so it
                                        // silently answers nothing for them:
                                        // `k.f(mk(4), true);` ran NO body while
                                        // `let _ = k.f(mk(4), true);` — which
                                        // reaches the shared walker's own
                                        // value-driven `Option`/`Result` arm —
                                        // ran one. Two spellings of one discard
                                        // disagreeing, with the bare one silent.
                                        //
                                        // The user-enum sibling is correct on
                                        // both spellings and stays on its
                                        // existing path; only the built-ins
                                        // move, which is the whole of the
                                        // difference measured.
                                        self.run_discarded_value_user_drops(discarded);
                                    } else {
                                        self.run_enum_payload_user_drops_value(&discarded);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    // B-2026-08-28-43 — a bare UNIT VARIANT in statement
                    // position, both spellings: `E.B;` (a `Path`) and `B;` (an
                    // `Identifier`). Neither is a `Call` or a `MethodCall`, so
                    // neither reached the arms above and an own-`impl Drop`
                    // enum's body ran on NO backend — the same shape
                    // B-2026-08-28-41 fixed one statement kind over, at
                    // `let _ = E.B`.
                    //
                    // The `Identifier` spelling goes through
                    // `fresh_bare_unit_variant_enum`, never a bare name lookup:
                    // the item pass seeds every unit variant into the outermost
                    // scope, so a plain `env.get` cannot tell the seeded
                    // constant from a LOCAL of enum type — and firing for a
                    // local would run a body its binding already owns.
                    ExprKind::Path { segments, .. }
                        if segments.len() == 2
                            && self
                                .qualified_enum_variant_is_unit(&segments[0], &segments[1])
                                .unwrap_or(false) =>
                    {
                        self.run_discarded_value_user_drops(discarded);
                    }
                    // B-2026-08-29-30 — the LITERAL arm, twin of codegen's
                    // `discarded_owned_literal_tail` leg at this same site.
                    // `R { .. };` and `(a, b);` in statement position reached
                    // no arm here at all, so no body ran — while
                    // `let _ = R { .. };` ran one through the wildcard gate.
                    // Judged by the same predicate that gate uses, so the two
                    // spellings cannot answer differently.
                    ExprKind::StructLiteral { .. } | ExprKind::Tuple(_)
                        if self.discard_rhs_produces_owned_value(expr, &discarded) =>
                    {
                        self.run_discarded_value_user_drops(discarded);
                    }
                    ExprKind::Identifier(n) if self.fresh_bare_unit_variant_enum(n).is_some() => {
                        self.run_discarded_value_user_drops(discarded);
                    }
                    // B-2026-08-31-35 — a bare value-position BLOCK in
                    // statement position (`{ W { r: t, b: 1 } };`). It runs no
                    // body of its own here — whatever already owned the tail's
                    // value still does — but the local its tail literal
                    // CONSUMED must not run a second one. Codegen reaches this
                    // shape because `discarded_match_value_tail` recurses
                    // through the wrapper; this is the same recursion on this
                    // backend, and without it the two disagreed on the block
                    // spelling alone while agreeing on `if`, `match` and the
                    // wildcard `let`.
                    ExprKind::Block(b) | ExprKind::Seq(b) | ExprKind::Unsafe(b)
                        if b.final_expr.as_deref().is_some_and(|t| {
                            self.discard_rhs_produces_owned_value(t, &discarded)
                        }) =>
                    {
                        self.disarm_discarded_tail_sources(expr);
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
                let unsigned_hint = self
                    .span_unsigned_int_width(&args[0].value.span)
                    .or_else(|| self.span_unsigned_int_width(&args[1].value.span));
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

/// B-2026-09-01-3 — the FIRST hop off a place expression's root, which is what
/// decides WHICH param-view record answers for it: a struct field consults
/// `param_view_struct_fields`, a tuple element `param_view_tuple_elems`. The
/// two are separate stores rather than one keyed by a hop, because they are
/// written by different constructors and invalidated independently; this type
/// exists only to carry the answer out of the chain walk in
/// `let_reads_param_view_field`.
enum FirstHop<'a> {
    Field(&'a str),
    Elem(usize),
}
