//! Codegen for `spawn()` / `TaskGroup.spawn()` / `TaskHandle.join()`.
//!
//! Phase 6 line 218 slice 4. Lowers the kara-source surface declared in
//! `runtime/stdlib/task_group.kara` (slice 1) into runtime-FFI calls
//! against `karac_runtime_spawn` / `_task_join` (slice 3 scheduler
//! module). See `runtime/src/scheduler.rs` for the runtime side of the
//! ABI.
//!
//! ## v1 dispatch shape
//!
//! At every `spawn(closure)` or `tg.spawn(closure)` call site, codegen
//! emits three artifacts:
//!
//! 1. A captured-environment struct type `{ T0_cap, T1_cap, ... }` —
//!    one field per free variable referenced by the closure body.
//! 2. A synthesized `extern "C" fn __spawn_wrap_N(env, result_out, cancel)`
//!    wrapper that loads captures from `env`, runs the closure body,
//!    stores the T-typed return value into `*result_out`, then frees
//!    the heap-allocated env and returns.
//! 3. At the outer call site: `malloc(sizeof(env_struct))`, copy
//!    captures from the outer scope into the heap env, call
//!    `karac_runtime_spawn(wrap_N, heap_env, sizeof(T), alignof(T))`,
//!    cast the returned `*KaracTaskHandle` to `i64`, and wrap into
//!    `TaskHandle { task_id: <i64> }`.
//!
//! The `cancel` parameter of the wrapper is presently unused (the
//! per-handle cancel-flag wiring lands with slice 5's `TaskGroup.drop`
//! fail-fast). v1 always reads it as `false` from the runtime side.
//!
//! ## TaskHandle layout
//!
//! `runtime/stdlib/task_group.kara` declares `struct TaskHandle[T] { task_id: i64 }`.
//! The single `i64` field holds the runtime-side `*mut KaracTaskHandle`
//! cast through `ptrtoint`. `.join()` reverses the cast and passes the
//! pointer back to `karac_runtime_task_join`.
//!
//! ## v1 limitations
//!
//! - **Closure must be a literal at the call site.** `spawn(|| body)`
//!   and `spawn(|conn| handle(conn))` are supported; `let f = || ...;
//!   spawn(f)` (bare-identifier closure) is deferred — needs the same
//!   indirect-call machinery as `compile_closure_call`.
//! - **`.join()` return type T derived from LHS annotation.** The
//!   typechecker doesn't bind T from the receiver's instantiated type
//!   (see slice 1's note about the `impl[T] TaskHandle[T] { fn m(self)
//!   -> T }` shape); the codegen mirrors by walking the enclosing
//!   function's AST for `let v: T = h.join()` and falling back to `i64`
//!   when no annotation is recoverable.

use crate::ast::*;
use crate::codegen::state::{CleanupAction, VarSlot};
use crate::ownership::OwnershipMode;
use crate::resolver::SpanKey;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicValueEnum, PointerValue};
use inkwell::AddressSpace;

impl<'ctx> super::Codegen<'ctx> {
    /// Lower `spawn(closure)` — for the free-fn shape. Returns a
    /// `TaskHandle { task_id: i64 }` struct value where `task_id` is
    /// the runtime-side `*mut KaracTaskHandle` cast to `i64`. The free
    /// shape does NOT register the child with any container — it
    /// produces an orphan task that the user must `.join()` explicitly
    /// (or accept that it outlives the function with `panic = "abort"`
    /// semantics on drop).
    pub(super) fn lower_spawn_call(
        &mut self,
        closure_expr: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        self.lower_spawn_shared(closure_expr, None)
    }

    /// Lower `tg.spawn(closure)` — the TaskGroup-method shape. Same
    /// machinery as the free `spawn()` plus a
    /// `karac_runtime_taskgroup_register(group, child_handle)` call
    /// before the wrap, so `tg`'s scope-exit drop sees the child in
    /// its registry. The receiver `tg_val` carries the group pointer
    /// in its `i64 id` field (cast back to pointer via `inttoptr`).
    pub(super) fn lower_taskgroup_spawn(
        &mut self,
        tg_val: BasicValueEnum<'ctx>,
        closure_expr: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Extract group_ptr from TaskGroup.id (i64 → ptr via inttoptr).
        let id_int = self
            .builder
            .build_extract_value(tg_val.into_struct_value(), 0, "tg.id")
            .unwrap()
            .into_int_value();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let group_ptr = self
            .builder
            .build_int_to_ptr(id_int, ptr_ty, "tg.ptr")
            .unwrap();
        self.lower_spawn_shared(closure_expr, Some(group_ptr.into()))
    }

    /// A2 slice 5a — if the spawn closure body is a **tail free-function
    /// call to a coroutine-compiled handler** (`spawn(|| handle(conn))`),
    /// return that handler's key. Only this shape gets the density-optimal
    /// non-blocking drive (`karac_runtime_spawn_coro` — worker ramps + returns,
    /// `TaskHandle` completion bound to the coroutine slot). The restriction
    /// is load-bearing: the ramp returns after the *first* suspend, so any
    /// code before or after the call would run while the coroutine is still
    /// parked. So we require the body to be *exactly* that call — directly, or
    /// wrapped in a trivial block (`{ handle(conn) }` / `{ handle(conn); }`).
    /// Method-handler, multi-statement, and non-unit-tail shapes fall back to
    /// the 2b.4 blocking spawn. See spike § 6⅞.
    fn spawn_coro_tail_fn_key(&self, body: &Expr) -> Option<String> {
        let call_expr: &Expr = match &body.kind {
            ExprKind::Block(b) => match (b.stmts.as_slice(), &b.final_expr) {
                ([], Some(e)) => e.as_ref(),
                ([only], None) => match &only.kind {
                    StmtKind::Expr(e) => e,
                    _ => return None,
                },
                _ => return None,
            },
            _ => body,
        };
        let name = match &call_expr.kind {
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Identifier(n) => n.clone(),
                ExprKind::Path { segments, .. } if segments.len() == 1 => segments[0].clone(),
                _ => return None,
            },
            _ => return None,
        };
        if self.is_coroutine_compiled(&name) {
            Some(name)
        } else {
            None
        }
    }

    /// Shared codegen for `spawn(closure)` and `tg.spawn(closure)`. The
    /// `group_ptr` parameter — when `Some(...)` — is registered with
    /// `karac_runtime_taskgroup_register` after the spawn FFI returns
    /// and before the `TaskHandle` wrap. `None` skips the registration
    /// (free-fn `spawn`).
    fn lower_spawn_shared(
        &mut self,
        closure_expr: &Expr,
        group_ptr: Option<BasicValueEnum<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // B-2026-06-17-2 — consume the discard flag now, before the closure
        // body is lowered into the wrapper, so a `spawn` nested in the body
        // doesn't inherit this site's detach. Emitted after the handle is
        // created and (for `tg.spawn`) registered.
        let detach_handle = std::mem::take(&mut self.pending_spawn_detach);
        let (params, body) = match &closure_expr.kind {
            ExprKind::Closure { params, body, .. } => (params.as_slice(), body.as_ref()),
            _ => {
                // Non-literal closure — for v1 slice 4 we don't recover
                // the closure's free-var list from a bare identifier.
                // Fall through to a no-op TaskHandle so the program
                // doesn't crash, but log a placeholder. A follow-on slice
                // wires bare-identifier closures through the existing
                // `closure_fn_types` registry.
                let i64_ty = self.context.i64_type();
                let task_handle_ty = self.context.struct_type(&[i64_ty.into()], false);
                let zero = i64_ty.const_int(0, false);
                let th_undef = task_handle_ty.get_undef();
                let th = self
                    .builder
                    .build_insert_value(th_undef, zero, 0, "task_handle.placeholder")
                    .unwrap()
                    .into_struct_value();
                return Ok(th.into());
            }
        };

        if !params.is_empty() {
            // Spawn closures take no params per design.md `Fn() -> T`.
            // Mismatched arity is the typechecker's job to reject; this
            // is a defensive guard against malformed AST.
            return Err(
                "spawn() closure must take no parameters (Fn() -> T per design.md)".to_string(),
            );
        }

        // 1. Collect captured free variables.
        let free_vars = self.collect_closure_free_vars(params, body);

        // Per-capture move/borrow classification — the cross-task aliasing fix.
        // A spawn closure captures its free vars; the wrapper/runtime treat a
        // MOVED (owned) capture as transferred into the task — the blocking
        // wrapper frees the heap buffer at task completion and the parent's
        // scope-exit free is suppressed. But a capture the body only *borrows*
        // (passes to a `ref` / `mut ref` param, reads a field) transfers no
        // ownership: the parent keeps it and frees it exactly once, after the
        // structured-concurrency join barrier — the same `Copy`-capture rule a
        // `par {}` branch already uses for a shared `Vec`, and the borrow
        // design.md § Structured Concurrency Lifetime Guarantees sanctions ("a
        // parallel task may legally borrow a binding ... the task joins strictly
        // before the cleanup fires"). Without this distinction a buffer captured
        // by N sibling tasks is freed N times — a double-free / use-after-free
        // (wrong reads plus an allocator "failed to lock mutex" abort).
        //
        // The ownership pass records the per-root capture mode in
        // `closure_capture_paths`. A root is a borrow iff it appears there and
        // every one of its paths is `Ref` / `MutRef` (no `Own`). Absent mode
        // data we fall back to the historical move treatment (correct for the
        // single-owner case, which is all the old path supported).
        let borrow_roots: std::collections::HashSet<String> = {
            let key = SpanKey::from_span(&closure_expr.span);
            match self.closure_capture_paths.get(&key) {
                Some(path_modes) => {
                    let mut seen: std::collections::HashSet<&str> =
                        std::collections::HashSet::new();
                    let mut moved: std::collections::HashSet<&str> =
                        std::collections::HashSet::new();
                    for (path, mode) in path_modes {
                        seen.insert(path.root.as_str());
                        if matches!(mode, OwnershipMode::Own) {
                            moved.insert(path.root.as_str());
                        }
                    }
                    free_vars
                        .iter()
                        .filter(|v| seen.contains(v.as_str()) && !moved.contains(v.as_str()))
                        .cloned()
                        .collect()
                }
                None => std::collections::HashSet::new(),
            }
        };

        // 2. Build env struct type. Empty captures still need a non-zero
        //    type for malloc; insert a sentinel i8 in that case (same
        //    convention as `compile_closure`).
        let env_field_types: Vec<BasicTypeEnum<'ctx>> = if free_vars.is_empty() {
            vec![self.context.i8_type().into()]
        } else {
            free_vars.iter().map(|n| self.variables[n].ty).collect()
        };
        let env_struct_ty = self.context.struct_type(&env_field_types, false);

        // 3. Infer return type T for sizeof/alignof at the spawn FFI.
        let return_ty = self.infer_closure_return_type(body, &std::collections::HashMap::new());
        let is_unit_return = matches!(
            return_ty,
            BasicTypeEnum::StructType(s) if s.count_fields() == 0
        );

        // A2 slice 5a — density-optimal non-blocking drive when the closure is
        // a tail coroutine-handler call. The wrapper then *ramps and returns*
        // (the coroutine call inside compiles to `ramp(args, slot)` with no
        // `park_slot_wait` — see the `coro_spawn_slot` intercept), and the
        // outer site calls `karac_runtime_spawn_coro` (binds the TaskHandle's
        // completion to the coroutine slot) instead of `karac_runtime_spawn`
        // (which would block a worker on the wrapper's nested wait).
        let use_coro_spawn = self.spawn_coro_tail_fn_key(body).is_some();

        // 4. Synthesize `__spawn_wrap_N(env, result_out, cancel)`.
        let id = self.closure_counter;
        self.closure_counter += 1;
        let wrapper_name = format!("__spawn_wrap_{}", id);

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let void_ty = self.context.void_type();
        let wrapper_ty = void_ty.fn_type(
            &[
                BasicMetadataTypeEnum::from(ptr_ty), // env
                BasicMetadataTypeEnum::from(ptr_ty), // result_out
                BasicMetadataTypeEnum::from(ptr_ty), // cancel
            ],
            false,
        );
        let wrapper_fn = self.module.add_function(&wrapper_name, wrapper_ty, None);

        // Save outer codegen state.
        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let saved_vars = std::mem::take(&mut self.variables);
        let saved_var_types = std::mem::take(&mut self.var_type_names);
        let saved_loop_stack = std::mem::take(&mut self.loop_stack);
        let saved_subst = std::mem::take(&mut self.type_subst);
        // The spawn wrapper is its own top-level function run on a pool worker —
        // NOT a continuation of any enclosing `par {}` branch. If this spawn
        // site sits inside an auto-parallelized statement group, `current_fn`
        // is a `__par_branch_*` fn and `branch_cancel_ptr` points at that
        // branch's cancel-flag argument; leaving it set would make the wrapper
        // body's per-call cancel checks (`emit_branch_cancel_check`) load that
        // arg from inside the wrapper — a cross-function reference that fails
        // verification ("argument in another function"). Clear it for the
        // wrapper body; restore below. (Surfaced flipping coroutines on by
        // default — `tg.spawn(coro); stmt` in a straight-line fn.)
        let saved_cancel = self.branch_cancel_ptr.take();

        // Build wrapper body.
        self.current_fn = Some(wrapper_fn);
        let entry = self.context.append_basic_block(wrapper_fn, "entry");
        self.builder.position_at_end(entry);

        let env_ptr = wrapper_fn.get_nth_param(0).unwrap().into_pointer_value();
        // Param 1 is `result_out` for an ordinary spawn; for the non-blocking
        // coroutine spawn (`use_coro_spawn`) the same slot is the runtime-owned
        // `KaracParkSlot` handed to the coroutine ramp (CoroSpawnFn ABI).
        let result_out = wrapper_fn.get_nth_param(1).unwrap().into_pointer_value();
        // `cancel` ptr unused until slice 5c (cooperative cancellation).
        let _cancel_ptr = wrapper_fn.get_nth_param(2).unwrap();

        // Load the env struct value through the env pointer.
        let env_val = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(env_struct_ty.into(), env_ptr, "__env")
            .unwrap();

        // Bind captures into self.variables for the body's compile.
        if !free_vars.is_empty() {
            for (i, var_name) in free_vars.iter().enumerate() {
                let cap_ty = env_field_types[i];
                let field_val = self
                    .builder
                    .build_extract_value(env_val.into_struct_value(), i as u32, var_name)
                    .unwrap();
                let alloca = self.create_entry_alloca(wrapper_fn, var_name, cap_ty);
                self.builder.build_store(alloca, field_val).unwrap();
                self.variables.insert(
                    var_name.clone(),
                    VarSlot {
                        ptr: alloca,
                        ty: cap_ty,
                    },
                );
                if let Some(type_name) = saved_var_types.get(var_name) {
                    self.var_type_names
                        .insert(var_name.clone(), type_name.clone());
                }
            }
        }

        // Channel-end captures: the spawned task OWNS a moved `Sender`/
        // `Receiver` and must drop it when the task finishes — a moved
        // `Sender`'s drop is what CLOSES the channel, waking a receiver that
        // blocked on `recv` (the producer-consumer terminator). The parent's
        // `DropChannelEnd` for these is suppressed at the outer site below
        // (otherwise the channel would be double-dropped, and worse, the
        // *close* would be owned by the parent thread — which may itself be
        // blocked on `recv`, deadlocking). Push a wrapper-scope cleanup frame
        // now (so its drops drain at task exit, AFTER the body's sends) and
        // register each captured channel end against its wrapper-local alloca.
        //
        // Detect channel-end captures from the parent's queued
        // `DropChannelEnd` (matched by the capture's PARENT alloca, still
        // present here — the suppression runs later at the outer site), NOT
        // from `var_type_names`/`saved_var_types`, which the statement-
        // hoisting pre-pass resets (the same volatility the dispatch gate
        // hit). `is_sender` rides along from the parent action.
        //
        // Heap-builtin (`String` / `Vec[T]`) captures need the symmetric
        // transfer. The parent's scope-exit `FreeVecBuffer` for the capture
        // is suppressed at the outer site below (so the parent never frees a
        // buffer the spawned task still reads — the loop UAF B-2026-06-18-8
        // closed), which hands buffer ownership to the task. But the wrapper
        // only *binds* the capture into a fresh alloca; it registers no
        // cleanup of its own, so whatever the body does not itself consume
        // leaks. A body that merely *borrows* the capture — `check(addr)`
        // whose `addr: String` param is inferred `ref` — moves nothing, so
        // without a wrapper-side free the buffer leaks once per spawn
        // (LeakSanitizer: N×`karac_string_clone` reachable from `main`; the
        // suppress-only B-2026-06-18-8 fix masked the UAF on macOS, where
        // there is no LSan, but converted it into this leak). Re-register the
        // parent's exact `FreeVecBuffer` (same `elem_ty` / element-drop
        // payload) against the wrapper-local alloca: the task frees the buffer
        // at completion, and if the body *does* move the capture into an
        // owning callee the standard move path zeros the source `cap`, so this
        // cleanup's `cap > 0` guard skips it (no double-free). Drains with the
        // channel-end frame at task exit (`drain_top_frame_with_emit` below),
        // before the env struct itself is freed.
        self.scope_cleanup_actions.push(Vec::new());
        let mut channel_caps: Vec<(PointerValue<'ctx>, bool)> = Vec::new();
        let mut vec_caps = Vec::new();
        for var_name in &free_vars {
            let Some(parent_slot) = saved_vars.get(var_name) else {
                continue;
            };
            let parent_ptr = parent_slot.ptr;
            let wrapper_ptr = self.variables[var_name].ptr;
            let mut is_sender = None;
            for frame in &self.scope_cleanup_actions {
                for action in frame {
                    match action {
                        CleanupAction::DropChannelEnd {
                            chan_alloca,
                            is_sender: s,
                        } if *chan_alloca == parent_ptr => {
                            is_sender = Some(*s);
                        }
                        // Only re-register the wrapper-side free for a MOVED
                        // capture. A borrowed capture stays owned by the parent
                        // (which frees it once after the join barrier), so the
                        // task must not free it — otherwise N sibling tasks each
                        // free the one shared buffer.
                        CleanupAction::FreeVecBuffer {
                            vec_alloca,
                            elem_ty,
                            elem_is_tensor,
                            elem_map_drop,
                            elem_agg_drop,
                        } if *vec_alloca == parent_ptr && !borrow_roots.contains(var_name) => {
                            vec_caps.push((
                                wrapper_ptr,
                                *elem_ty,
                                *elem_is_tensor,
                                elem_map_drop.clone(),
                                *elem_agg_drop,
                            ));
                        }
                        _ => {}
                    }
                }
            }
            if let Some(s) = is_sender {
                channel_caps.push((wrapper_ptr, s));
            }
        }
        for (wrapper_alloca, is_sender) in channel_caps {
            self.track_channel_var(wrapper_alloca, is_sender);
        }
        // For the non-blocking coroutine spawn (`use_coro_spawn`), the wrapper
        // ramps the coroutine and RETURNS while the coroutine is still parked
        // at its first suspend — its `drain_top_frame_with_emit` below runs
        // long before the coroutine finishes. So a `FreeVecBuffer` re-registered
        // here would free a moved-in `String`/`Vec` capture's buffer out from
        // under the still-running coroutine — a use-after-free (empty proxied
        // response / silent connect failure when the freed buffer is the
        // upstream address). The coroutine receives the capture BY VALUE through
        // the ramp args and owns it; it frees the buffer at ITS completion
        // (body-end + per-park destroy edge), exactly as it already owns its
        // moved-in `UserDrop` and channel-end params (see the coroutine param
        // registration in `compile_function_body`). So drop the wrapper-side
        // re-registration on this path — only the blocking spawn (`spawn`
        // wrapper IS the task, runs to completion) needs it. Regression:
        // `tests/relay_bench.rs` + the multi-capture coroutine-spawn cases in
        // `tests/coro_e2e.rs`.
        if use_coro_spawn {
            vec_caps.clear();
        }
        for (vec_alloca, elem_ty, elem_is_tensor, elem_map_drop, elem_agg_drop) in vec_caps {
            if let Some(frame) = self.scope_cleanup_actions.last_mut() {
                frame.push(CleanupAction::FreeVecBuffer {
                    vec_alloca,
                    elem_ty,
                    elem_is_tensor,
                    elem_map_drop,
                    elem_agg_drop,
                });
            }
        }

        // Compile the closure body inside the wrapper context. For the
        // non-blocking coroutine spawn, flip the call-site intercept into
        // "ramp with this runtime-owned slot, don't wait" mode for the
        // duration of the body emission (`result_out` here IS the slot).
        let saved_spawn_slot = self.coro_spawn_slot;
        if use_coro_spawn {
            self.coro_spawn_slot = Some(result_out);
        }
        let result = self.compile_expr(body)?;
        self.coro_spawn_slot = saved_spawn_slot;

        // Store the result into *result_out (ordinary spawn only). A coroutine
        // spawn returns unit and `result_out` is the completion slot, not a
        // result buffer — nothing to store.
        if !use_coro_spawn && !is_unit_return {
            // result_out can legally be null when result_size == 0;
            // codegen-side wrappers always pass a non-null buffer for
            // non-unit returns, but skip the store if the buffer is
            // null for robustness against future callers.
            let null_ptr = ptr_ty.const_null();
            let is_null = self
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::EQ,
                    self.builder
                        .build_ptr_to_int(result_out, self.context.i64_type(), "ro.addr")
                        .unwrap(),
                    self.builder
                        .build_ptr_to_int(null_ptr, self.context.i64_type(), "null.addr")
                        .unwrap(),
                    "ro.is_null",
                )
                .unwrap();
            let store_block = self.context.append_basic_block(wrapper_fn, "ro.store");
            let skip_block = self.context.append_basic_block(wrapper_fn, "ro.skip");
            self.builder
                .build_conditional_branch(is_null, skip_block, store_block)
                .unwrap();
            self.builder.position_at_end(store_block);
            self.builder.build_store(result_out, result).unwrap();
            self.builder.build_unconditional_branch(skip_block).unwrap();
            self.builder.position_at_end(skip_block);
        }

        // Drain the wrapper-scope channel-end-capture frame: the task is
        // finishing, so drop each captured `Sender`/`Receiver` now (a last
        // `Sender` drop closes the channel + wakes blocked receivers). Runs
        // after the body's sends, before the env free.
        self.drain_top_frame_with_emit();

        // Free the heap-allocated env before returning.
        let free_fn = self
            .module
            .get_function("free")
            .expect("free declared in Codegen::new");
        self.builder
            .build_call(free_fn, &[env_ptr.into()], "")
            .unwrap();
        self.builder.build_return(None).unwrap();

        // Restore outer state.
        self.type_subst = saved_subst;
        self.loop_stack = saved_loop_stack;
        self.var_type_names = saved_var_types;
        self.variables = saved_vars;
        self.current_fn = saved_fn;
        self.branch_cancel_ptr = saved_cancel;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }

        // 5. Outer site: malloc env, populate, call karac_runtime_spawn.
        let outer_fn = self
            .current_fn
            .expect("spawn() called outside any function context");

        let env_size_const = env_struct_ty
            .size_of()
            .expect("env_struct_ty has a static size");
        let malloc_fn = self
            .module
            .get_function(crate::codegen::driver::c_malloc_symbol())
            .expect("malloc declared in Codegen::new");
        let heap_env_call = self
            .builder
            .build_call(malloc_fn, &[env_size_const.into()], "__spawn_env")
            .unwrap();
        let heap_env = heap_env_call
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        // Populate the heap env with current values of captured vars.
        if !free_vars.is_empty() {
            let mut env_agg = env_struct_ty.get_undef();
            for (i, var_name) in free_vars.iter().enumerate() {
                let slot = self.variables[var_name];
                let val = self
                    .builder
                    .build_load(slot.ty, slot.ptr, var_name)
                    .unwrap();
                env_agg = self
                    .builder
                    .build_insert_value(env_agg, val, i as u32, "__env_field")
                    .unwrap()
                    .into_struct_value();
            }
            self.builder.build_store(heap_env, env_agg).unwrap();

            // Move-suppression for the captured values. A spawn closure is
            // `OnceFn() -> T`: it captures its free vars *by move*, taking
            // ownership into the spawned task (the cross-task-safe check at
            // the spawn site already verified this is sound). The values
            // were just bitwise-copied into the heap env above; the spawned
            // wrapper now owns them and runs their `Drop` when the task
            // finishes. So the parent must NOT also drop them at scope exit
            // — without this, a captured resource with a user `Drop` (e.g.
            // a `WebSocket`, whose Drop closes the fd) is dropped twice:
            // once by the parent and once by the task. For an fd that is a
            // double-`close()`, which corrupts the fd table (a reused fd
            // gets closed out from under another connection) and, on macOS,
            // can wedge `close()` itself when the fd is concurrently
            // registered in the kqueue by the task's recv park — the
            // intermittent accept-loop hang the `ws_idle_holder` demo hit
            // at scale. Suppressing is a no-op for `Copy` captures (they
            // have no `UserDrop` cleanup entry), so it is safe to apply to
            // every capture.
            for var_name in &free_vars {
                self.suppress_user_drop_for_var(var_name);
                // Channel ends moved into the task: the wrapper now owns the
                // drop (registered above), so the parent must not also drop —
                // the parent's drop would double-free and, for a `Sender`,
                // hand channel-*close* ownership to a thread that may be
                // blocked on `recv` (deadlock). Mirrors the user-Drop
                // suppression; a no-op for non-channel captures.
                self.suppress_channel_drop_for_var(var_name);
                // Heap builtins (`String` / `Vec[T]`) moved into the task: the
                // `{data, len, cap}` header was bitwise-copied into the env
                // above, so the wrapper's env now owns the buffer and frees it
                // at task completion. The parent must NOT also free it. Its
                // `FreeVecBuffer` keys on the alloca (not a name), so the
                // user/channel suppressors above miss it. Without this, a
                // capture moved inside a LOOP is freed at loop-body scope exit
                // while the spawned coroutine still reads it — a use-after-free.
                // A single non-loop spawn masks the bug because the `TaskGroup`
                // join at the enclosing scope precedes the parent's free; a
                // loop drains each iteration's frame first. This closes the
                // same double-ownership hole for plain heap collections — the
                // canonical `loop { let s = …; tg.spawn(|| use(s)) }`
                // server-handler shape.
                //
                // Only for a MOVED capture (or the coroutine spawn, which always
                // takes the buffer by value and frees it at its own completion).
                // A BORROWED capture on the blocking path is left owned by the
                // parent — it frees the buffer once after the join barrier — so
                // suppressing here would leak it, and N sibling borrows of the
                // same buffer would each have freed it (double-free) had the
                // wrapper re-registration above not also been skipped.
                if use_coro_spawn || !borrow_roots.contains(var_name) {
                    self.suppress_vec_buffer_drop_for_var(var_name);
                }
            }
        }

        // Compute (result_size, result_align) for the runtime FFI. The
        // runtime allocates the result buffer; the wrapper writes T's
        // bytes into it.
        let target_data = self.ensure_target_data()?;
        let (result_size, result_align) = if is_unit_return {
            (0u64, 1u64)
        } else {
            let size = target_data.get_store_size(&return_ty);
            let align = target_data.get_abi_alignment(&return_ty) as u64;
            (size, align)
        };
        let usize_ty = if std::mem::size_of::<usize>() == 8 {
            self.context.i64_type()
        } else {
            self.context.i32_type()
        };

        let wrapper_ptr = wrapper_fn.as_global_value().as_pointer_value();
        let handle_call = if use_coro_spawn {
            // A2 slice 5a — non-blocking: the runtime allocates the bound
            // completion slot and enqueues a worker that ramps + returns; the
            // wrapper takes `(env, slot, cancel)` (CoroSpawnFn) so no result
            // size/align is threaded (coroutine handlers return unit).
            let spawn_coro_fn = self
                .module
                .get_function("karac_runtime_spawn_coro")
                .expect("karac_runtime_spawn_coro declared in Codegen::new");
            self.builder
                .build_call(
                    spawn_coro_fn,
                    &[wrapper_ptr.into(), heap_env.into()],
                    "__spawn_coro_handle",
                )
                .unwrap()
        } else {
            let spawn_fn = self
                .module
                .get_function("karac_runtime_spawn")
                .expect("karac_runtime_spawn declared in Codegen::new");
            self.builder
                .build_call(
                    spawn_fn,
                    &[
                        wrapper_ptr.into(),
                        heap_env.into(),
                        usize_ty.const_int(result_size, false).into(),
                        usize_ty.const_int(result_align, false).into(),
                    ],
                    "__spawn_handle",
                )
                .unwrap()
        };
        let handle_ptr = handle_call
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let _ = outer_fn; // touched for assertion clarity above

        // Phase 6 line 218 slice 5: register the child with the
        // TaskGroup so drop can wait for it. Skipped for free `spawn`
        // (no container to register with).
        if let Some(g) = group_ptr {
            let register_fn = self
                .module
                .get_function("karac_runtime_taskgroup_register")
                .expect("karac_runtime_taskgroup_register declared in Codegen::new");
            let _ = self
                .builder
                .build_call(register_fn, &[g.into(), handle_ptr.into()], "")
                .unwrap();
        }

        // B-2026-06-17-2 — a discarded handle (bare `spawn(...);` /
        // `tg.spawn(...);`) is never bound or joined, so nothing will ever free
        // the runtime handle through the join path. Mark it detached so the
        // runtime eager-reaps it: a free-spawn handle self-reaps on completion;
        // a `tg.spawn` child is reaped by the group's register-time sweep.
        // Emitted *after* register so the child is already `registered` when
        // detach runs — the detach path checks that flag to leave a group child
        // for the sweep rather than self-freeing it.
        if detach_handle {
            let detach_fn = self
                .module
                .get_function("karac_runtime_task_detach")
                .expect("karac_runtime_task_detach declared in Codegen::new");
            let _ = self
                .builder
                .build_call(detach_fn, &[handle_ptr.into()], "")
                .unwrap();
        }

        // 6. Wrap into `TaskHandle { task_id: handle_ptr as i64 }`.
        let i64_ty = self.context.i64_type();
        let task_id = self
            .builder
            .build_ptr_to_int(handle_ptr, i64_ty, "task_id")
            .unwrap();
        let task_handle_ty = self.context.struct_type(&[i64_ty.into()], false);
        let th_undef = task_handle_ty.get_undef();
        let th = self
            .builder
            .build_insert_value(th_undef, task_id, 0, "task_handle")
            .unwrap()
            .into_struct_value();

        Ok(th.into())
    }

    /// Recover the LLVM lowering of `T` for a `TaskHandle[T].join()` call.
    /// The typechecker records `T` for every join site in
    /// `task_join_return_types` (keyed by the join MethodCall span); this
    /// looks it up and lowers it, so a non-scalar return (a `Vec`/`String`/
    /// struct from `spawn`) is sized correctly. Falls back to `i64` only when
    /// the site wasn't recorded — e.g. the canonical accept-loop pattern
    /// (`tg.spawn(|| handle_client(conn))`) discards the handle with no
    /// `.join()` call at all, so the default is never observed there.
    ///
    /// History: this returned `i64` unconditionally at slice 4 (a documented
    /// limitation), which made `join()` on a heap return read `i64`-shaped
    /// bytes off the result buffer — the value came back as garbage and
    /// trapped. Fixed by threading the typechecker's recorded `T` through
    /// `task_join_return_types` (surfaced by the Fathom dogfood — a parallel
    /// Mandelbrot whose row-bands return `Vec[u8]`).
    pub(super) fn recover_task_handle_join_return_ty(
        &self,
        call_span: &crate::token::Span,
    ) -> BasicTypeEnum<'ctx> {
        if let Some(te) = self
            .task_join_return_types
            .get(&(call_span.offset, call_span.length))
        {
            return self.llvm_type_for_type_expr(te);
        }
        self.context.i64_type().into()
    }

    /// Lower `h.join()` where `h: TaskHandle[T]`. Extracts the runtime
    /// handle pointer from `h.task_id`, allocates an out-slot for T,
    /// calls `karac_runtime_task_join(handle, out_slot)`, loads T from
    /// the slot and returns it.
    ///
    /// `return_ty_hint` is the LLVM lowering of T as recovered from the
    /// call-site context (typically `let v: T = h.join()`). When the
    /// caller can't recover T, falls back to `i64` — the runtime side's
    /// `karac_runtime_task_join` uses `result_size == 0` to skip the
    /// memcpy for unit-returning tasks, so an i64-shaped read on a
    /// unit-returning handle is safe (the i64 will be garbage but the
    /// caller's stub-body type contract makes the value unobservable).
    pub(super) fn lower_task_handle_join(
        &mut self,
        self_val: BasicValueEnum<'ctx>,
        return_ty: BasicTypeEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // 1. Extract task_id (i64) from the TaskHandle struct.
        let task_id_int = self
            .builder
            .build_extract_value(self_val.into_struct_value(), 0, "task_id")
            .unwrap()
            .into_int_value();

        // 2. Cast i64 → *mut KaracTaskHandle.
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let handle_ptr = self
            .builder
            .build_int_to_ptr(task_id_int, ptr_ty, "handle_ptr")
            .unwrap();

        // 3. Allocate out_slot of return_ty in the current fn's entry.
        let outer_fn = self
            .current_fn
            .expect("TaskHandle.join() called outside any function context");
        let out_slot = self.create_entry_alloca(outer_fn, "__join_out", return_ty);

        // 4. Call karac_runtime_task_join(handle, out_slot).
        let join_fn = self
            .module
            .get_function("karac_runtime_task_join")
            .expect("karac_runtime_task_join declared in Codegen::new");
        let _ = self
            .builder
            .build_call(
                join_fn,
                &[handle_ptr.into(), out_slot.into()],
                "__join_status",
            )
            .unwrap();

        // 5. Load T from out_slot and return.
        let result = self
            .builder
            .build_load(return_ty, out_slot, "__join_result")
            .unwrap();
        Ok(result)
    }

    /// A2 slice 5b-1 — lower `tg.cancel()` to
    /// `karac_runtime_taskgroup_cancel(group_ptr)`. Extracts the runtime
    /// group pointer from `TaskGroup.id` (same `i64 → ptr` cast as
    /// `lower_taskgroup_spawn`) and calls the FFI, which flips every
    /// registered child's per-task cancel flag. Returns unit.
    ///
    /// Inert at this slice: the flag is set but the dispatcher does not yet
    /// route it to parked coroutines (slice 5c wires that). See the runtime
    /// `karac_runtime_taskgroup_cancel` doc + the spike § 6⅞.
    pub(super) fn lower_taskgroup_cancel(
        &mut self,
        tg_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Extract group_ptr from TaskGroup.id (i64 → ptr via inttoptr) — the
        // same shape lower_taskgroup_spawn uses.
        let id_int = self
            .builder
            .build_extract_value(tg_val.into_struct_value(), 0, "tg.id")
            .unwrap()
            .into_int_value();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let group_ptr = self
            .builder
            .build_int_to_ptr(id_int, ptr_ty, "tg.ptr")
            .unwrap();

        let cancel_fn = self
            .module
            .get_function("karac_runtime_taskgroup_cancel")
            .expect("karac_runtime_taskgroup_cancel declared in Codegen::new");
        let _ = self
            .builder
            .build_call(cancel_fn, &[group_ptr.into()], "")
            .unwrap();

        // `cancel()` returns unit — lowered as `i64 0` per the codegen unit
        // convention (see types_lowering.rs "unit type → i64 zero").
        Ok(self.context.i64_type().const_zero().into())
    }
}
