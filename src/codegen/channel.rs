//! Channel-end method codegen — `Sender.send` / `Sender.clone` /
//! `Receiver.recv` / `Receiver.try_recv`.
//!
//! `Sender[T]` / `Receiver[T]` lower to the opaque `*mut KaracChannel`
//! pointer returned by `Channel.new()` (see `assoc_call.rs`); the queue and
//! refcount live in `runtime/src/channel.rs`. These methods marshal the
//! type-erased payload across the `karac_runtime_channel_*` FFI: the element
//! type `T` is recovered from the typechecker's `channel_elem_types`
//! side-table (keyed by the MethodCall span), lowered to its LLVM shape, and
//! its store size threaded as the per-call `elem_size`.
//!
//! v1 floor (mirrors the interpreter, non-blocking): `recv` returns the
//! zero-value of `T` on an empty queue (the runtime zero-fills the out slot),
//! so its lowering ignores the FFI presence discriminant; `try_recv` reads
//! the discriminant to build `Some`/`None`.

use crate::ast::*;

use inkwell::values::BasicValueEnum;
use inkwell::IntPredicate;

impl<'ctx> super::Codegen<'ctx> {
    pub(super) fn compile_channel_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        match method {
            "send" => self.compile_channel_send(object, args, call_span),
            "recv" => self.compile_channel_recv(object, call_span),
            "try_recv" => self.compile_channel_try_recv(object, call_span),
            "clone" => self.compile_channel_clone(object),
            "__schedule_after" => self.compile_channel_schedule_after(object, args),
            "__schedule_every" => self.compile_channel_schedule_every(object, args),
            "__schedule_animation_frames" => {
                self.compile_channel_schedule_animation_frames(object, args)
            }
            "__schedule_pointer_moves" => self.compile_channel_schedule_pointer_moves(object, args),
            "__schedule_wheel" => self.compile_channel_schedule_wheel(object, args),
            "__schedule_keydown" => self.compile_channel_schedule_keydown(object, args),
            "__schedule_keyup" => self.compile_channel_schedule_keyup(object, args),
            "__schedule_clicks" => self.compile_channel_schedule_clicks(object, args),
            "__schedule_dblclick" => self.compile_channel_schedule_dblclick(object, args),
            "__schedule_resize" => self.compile_channel_schedule_resize(object, args),
            "__schedule_contextmenu" => self.compile_channel_schedule_contextmenu(object, args),
            // The dispatch gate in `compile_method_call` only routes the
            // methods above here.
            _ => unreachable!("compile_channel_method: unexpected method `{method}`"),
        }
    }

    /// Resolve the channel element type `T` for a channel-op call site to its
    /// `(LLVM type, store-size-in-bytes)`. The size feeds the type-erased
    /// `karac_runtime_channel_*` `elem_size` parameter. Missing means the
    /// typechecker didn't record the site — a contract break worth surfacing
    /// loudly rather than silently sending zero bytes.
    pub(super) fn channel_elem_ty_and_size(
        &mut self,
        call_span: &crate::token::Span,
    ) -> Result<(inkwell::types::BasicTypeEnum<'ctx>, u64), String> {
        let te = self
            .channel_elem_types
            .get(&(call_span.offset, call_span.length))
            .cloned()
            .ok_or_else(|| {
                "channel op missing element type — typechecker `infer_channel_method` should \
                 populate `channel_elem_types` at every send/recv/try_recv site"
                    .to_string()
            })?;
        let elem_ty = self.llvm_type_for_type_expr(&te);
        let elem_size = self.ensure_target_data()?.get_store_size(&elem_ty);
        Ok((elem_ty, elem_size))
    }

    /// `tx.send(v)` → spill `v` to a stack slot and `karac_runtime_channel_send`
    /// its `elem_size` bytes onto the queue. Returns Unit.
    fn compile_channel_send(
        &mut self,
        object: &Expr,
        args: &[CallArg],
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 1 {
            return Err("Sender.send expects exactly one argument".to_string());
        }
        let (elem_ty, elem_size) = self.channel_elem_ty_and_size(call_span)?;
        let fn_val = self.current_fn.unwrap();
        // Receiver first (source order), then the value.
        let ch = self.compile_expr(object)?.into_pointer_value();
        let arg_val = self.compile_expr(&args[0].value)?;
        let slot = self.create_entry_alloca(fn_val, "chan.send.val", elem_ty);
        self.builder.build_store(slot, arg_val).unwrap();

        let size_const = self.context.i64_type().const_int(elem_size, false);
        let send_fn = self
            .module
            .get_function("karac_runtime_channel_send")
            .expect("karac_runtime_channel_send declared in Codegen::new");
        self.builder
            .build_call(
                send_fn,
                &[ch.into(), slot.into(), size_const.into()],
                "chan.send",
            )
            .unwrap();
        // Unit return (the i64-zero unit value).
        Ok(self.context.i64_type().const_zero().into())
    }

    /// `rx.recv()` → `karac_runtime_channel_recv` into a stack slot, then load
    /// `T`. The presence discriminant is ignored: on an empty queue the
    /// runtime zero-fills the slot, which is the floor's "empty" answer for a
    /// `-> T` result (mirrors the interpreter's `unwrap_or(Unit)`).
    fn compile_channel_recv(
        &mut self,
        object: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let (elem_ty, elem_size) = self.channel_elem_ty_and_size(call_span)?;
        let fn_val = self.current_fn.unwrap();
        let ch = self.compile_expr(object)?.into_pointer_value();
        let out = self.create_entry_alloca(fn_val, "chan.recv.out", elem_ty);

        let size_const = self.context.i64_type().const_int(elem_size, false);
        let recv_fn = self
            .module
            .get_function("karac_runtime_channel_recv")
            .expect("karac_runtime_channel_recv declared in Codegen::new");
        self.builder
            .build_call(
                recv_fn,
                &[ch.into(), out.into(), size_const.into()],
                "chan.recv.found",
            )
            .unwrap();
        let val = self
            .builder
            .build_load(elem_ty, out, "chan.recv.val")
            .unwrap();
        Ok(val)
    }

    /// `rx.try_recv()` → `Option[T]`. Routes through the **non-blocking**
    /// `karac_runtime_channel_try_recv` (NOT the blocking `recv`); the
    /// presence discriminant drives a `Some`/`None` build (the Map.get
    /// template).
    fn compile_channel_try_recv(
        &mut self,
        object: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let (elem_ty, elem_size) = self.channel_elem_ty_and_size(call_span)?;
        let fn_val = self.current_fn.unwrap();
        let ch = self.compile_expr(object)?.into_pointer_value();
        let out = self.create_entry_alloca(fn_val, "chan.tryrecv.out", elem_ty);

        let size_const = self.context.i64_type().const_int(elem_size, false);
        let recv_fn = self
            .module
            .get_function("karac_runtime_channel_try_recv")
            .expect("karac_runtime_channel_try_recv declared in Codegen::new");
        let found_i8 = self
            .builder
            .build_call(
                recv_fn,
                &[ch.into(), out.into(), size_const.into()],
                "chan.tryrecv.found",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let zero_i8 = self.context.i8_type().const_zero();
        let found = self
            .builder
            .build_int_compare(IntPredicate::NE, found_i8, zero_i8, "chan.tryrecv.some")
            .unwrap();

        let some_bb = self
            .context
            .append_basic_block(fn_val, "chan.tryrecv.some.bb");
        let none_bb = self
            .context
            .append_basic_block(fn_val, "chan.tryrecv.none.bb");
        let merge_bb = self
            .context
            .append_basic_block(fn_val, "chan.tryrecv.merge");
        self.builder
            .build_conditional_branch(found, some_bb, none_bb)
            .unwrap();

        // Some — load the value and split into Option's 3 payload words.
        self.builder.position_at_end(some_bb);
        let val = self
            .builder
            .build_load(elem_ty, out, "chan.tryrecv.val")
            .unwrap();
        let some_words = self.coerce_to_payload_words(val, 3)?;
        let some_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        // None.
        self.builder.position_at_end(none_bb);
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        // Merge — phi the words and assemble the Option aggregate.
        self.builder.position_at_end(merge_bb);
        let agg =
            self.build_option_some_via_phis(&some_words, some_end_bb, none_bb, "chan.tryrecv");
        Ok(agg)
    }

    /// `tx.__schedule_after(ms)` — the compiler builtin backing
    /// `std.web.time.after` (phase-10 host-async timer producers). Registers
    /// a host `setTimeout` that will send `()` on the channel after `ms`,
    /// then close it. To keep the channel open across `after`'s return (the
    /// local `tx` is dropped at scope exit), it **clones** the channel
    /// reference and hands the clone to the host via the `kara_host`
    /// `__kara_timer_after(ch: i64, ms: i64)` import; the host owns that
    /// reference and drops it (after sending) by calling
    /// `karac_runtime_channel_drop_sender`. The channel pointer crosses the
    /// host boundary as an `i64` handle (the `__karac_malloc64` size_t
    /// discipline — i64 for every target). Returns Unit.
    ///
    /// Only reachable on `wasm_browser --features wasm-threads`: `after`
    /// declares `writes(Timer)` (target-gated to `wasm_browser`), and the
    /// sequential default is rejected pre-codegen by the host-async gate.
    fn compile_channel_schedule_after(
        &mut self,
        object: &Expr,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 1 {
            return Err("Sender.__schedule_after expects exactly one argument".to_string());
        }
        // Host-async producer gate (phase-10): a host `setTimeout` callback
        // can only wake a *blocked* `recv` on a target with a thread to
        // block off the main event loop — `--features wasm-threads`. On the
        // sequential default the channel could never be fed (a single thread
        // cannot both park in `recv` and run the host event loop), so reject
        // it here with a clear pointer rather than emit an unsatisfiable
        // `__kara_timer_after` import that fails opaquely at instantiate.
        // Build-time only (codegen) so `karac check` — which has no
        // `--features` flag and never reaches codegen — does not
        // false-reject a program destined for a threaded build.
        if crate::target::active_target_is_wasm() && !crate::target::wasm_threads_enabled() {
            return Err(
                "std.web.time.after (host-async timer) requires `--features wasm-threads` on \
                 this target: a sequential WASM build has no thread to block in `recv` while \
                 the host event loop runs the timer, so the channel could never be fed. \
                 Rebuild with `--target=wasm_browser --features wasm-threads` (design.md \
                 § Scheduler contract on WASM — Realization status)."
                    .to_string(),
            );
        }
        let ch = self.compile_expr(object)?.into_pointer_value();
        let ms = self.compile_expr(&args[0].value)?.into_int_value();

        // Clone first — the surviving reference is what keeps the channel
        // open until the host fires + drops it.
        let clone_fn = self
            .module
            .get_function("karac_runtime_channel_clone")
            .expect("karac_runtime_channel_clone declared in Codegen::new");
        let cloned = self
            .builder
            .build_call(clone_fn, &[ch.into()], "timer.chan.clone")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let i64_ty = self.context.i64_type();
        let ch_i64 = self
            .builder
            .build_ptr_to_int(cloned, i64_ty, "timer.chan.i64")
            .unwrap();

        let host_fn = self.get_or_declare_timer_after_import();
        self.builder
            .build_call(host_fn, &[ch_i64.into(), ms.into()], "timer.after")
            .unwrap();
        // Unit return (the i64-zero unit value).
        Ok(self.context.i64_type().const_zero().into())
    }

    /// `tx.__schedule_every(ms)` — the compiler builtin backing
    /// `std.web.time.every`. The `after` arg-shape (takes the period in ms)
    /// crossed with the `animation_frames` lifetime: *multi-shot* — the host
    /// keeps a `setInterval` armed and feeds `()` on the channel every `ms`,
    /// so a `loop { ticks.recv(); … }` over the result runs once per period.
    /// The host owns the surviving cloned sender for the interval's lifetime
    /// and **never** drops it (unlike `after`, which drops after its single
    /// fire), so the channel stays open across ticks. Same `--features
    /// wasm-threads` gate (a sequential build has no thread to park in `recv`
    /// while the host event loop runs the interval).
    fn compile_channel_schedule_every(
        &mut self,
        object: &Expr,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 1 {
            return Err("Sender.__schedule_every expects exactly one argument".to_string());
        }
        if crate::target::active_target_is_wasm() && !crate::target::wasm_threads_enabled() {
            return Err(
                "std.web.time.every (host-async interval) requires `--features wasm-threads` on \
                 this target: a sequential WASM build has no thread to block in `recv` while the \
                 host event loop runs the interval, so the channel could never be fed. Rebuild \
                 with `--target=wasm_browser --features wasm-threads` (design.md § Scheduler \
                 contract on WASM — Realization status)."
                    .to_string(),
            );
        }
        let ch = self.compile_expr(object)?.into_pointer_value();
        let ms = self.compile_expr(&args[0].value)?.into_int_value();

        // Clone first — the surviving reference keeps the channel open across
        // every tick (the local `tx` in `every` drops at return).
        let clone_fn = self
            .module
            .get_function("karac_runtime_channel_clone")
            .expect("karac_runtime_channel_clone declared in Codegen::new");
        let cloned = self
            .builder
            .build_call(clone_fn, &[ch.into()], "every.chan.clone")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let i64_ty = self.context.i64_type();
        let ch_i64 = self
            .builder
            .build_ptr_to_int(cloned, i64_ty, "every.chan.i64")
            .unwrap();

        let host_fn = self.get_or_declare_timer_every_import();
        self.builder
            .build_call(host_fn, &[ch_i64.into(), ms.into()], "timer.every")
            .unwrap();
        Ok(self.context.i64_type().const_zero().into())
    }

    /// `tx.__schedule_animation_frames()` — the compiler builtin backing
    /// `std.web.time.animation_frames`. Like `__schedule_after` but
    /// *multi-shot*: it hands the host a `requestAnimationFrame` loop that
    /// re-arms itself and feeds `()` on the channel once per frame, so a
    /// `loop { frames.recv(); … }` over the result is a render loop. The host
    /// owns the surviving cloned sender for the lifetime of the loop and never
    /// drops it (the channel stays open across frames). Takes no argument; the
    /// frame cadence is the host's display refresh. Same `--features
    /// wasm-threads` gate as `__schedule_after` (a sequential build has no
    /// thread to park in `recv` while the rAF callback runs).
    fn compile_channel_schedule_animation_frames(
        &mut self,
        object: &Expr,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if !args.is_empty() {
            return Err("Sender.__schedule_animation_frames expects no arguments".to_string());
        }
        if crate::target::active_target_is_wasm() && !crate::target::wasm_threads_enabled() {
            return Err(
                "std.web.time.animation_frames (host-async frame loop) requires `--features \
                 wasm-threads` on this target: a sequential WASM build has no thread to block in \
                 `recv` while the host event loop runs requestAnimationFrame, so the channel \
                 could never be fed. Rebuild with `--target=wasm_browser --features wasm-threads` \
                 (design.md § Scheduler contract on WASM — Realization status)."
                    .to_string(),
            );
        }
        // Clone first — the surviving reference keeps the channel open across
        // every frame (the local `tx` in `animation_frames` drops at return).
        let ch = self.compile_expr(object)?.into_pointer_value();
        let clone_fn = self
            .module
            .get_function("karac_runtime_channel_clone")
            .expect("karac_runtime_channel_clone declared in Codegen::new");
        let cloned = self
            .builder
            .build_call(clone_fn, &[ch.into()], "raf.chan.clone")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let i64_ty = self.context.i64_type();
        let ch_i64 = self
            .builder
            .build_ptr_to_int(cloned, i64_ty, "raf.chan.i64")
            .unwrap();

        let host_fn = self.get_or_declare_animation_frames_import();
        self.builder
            .build_call(host_fn, &[ch_i64.into()], "raf.schedule")
            .unwrap();
        Ok(self.context.i64_type().const_zero().into())
    }

    /// `tx.__schedule_pointer_moves()` — the compiler builtin backing
    /// `std.web.events.pointer_moves`. Like `__schedule_animation_frames`
    /// (multi-shot, host owns the surviving cloned sender for the listener's
    /// life), but the host feeds a *non-unit* payload: it marshals each
    /// `pointermove`'s coordinates into the service instance's
    /// `karac_runtime_event_scratch()` buffer and `channel_send`s a
    /// `PointerEvent` ({ x: f64, y: f64 }, 16 bytes) — vs the zero-byte `()`
    /// a timer/frame producer sends. This is the first `Channel[T]`, `T != ()`
    /// host-async producer; the cross-instance payload allocation/copy is the
    /// surface the unit-channel slice deliberately sidestepped. Takes no
    /// argument. Same `--features wasm-threads` gate as the timer/frame
    /// builtins (a sequential build has no thread to park in `recv` while the
    /// host event loop dispatches pointer events).
    fn compile_channel_schedule_pointer_moves(
        &mut self,
        object: &Expr,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if !args.is_empty() {
            return Err("Sender.__schedule_pointer_moves expects no arguments".to_string());
        }
        if crate::target::active_target_is_wasm() && !crate::target::wasm_threads_enabled() {
            return Err(
                "std.web.events.pointer_moves (host-async input stream) requires `--features \
                 wasm-threads` on this target: a sequential WASM build has no thread to block in \
                 `recv` while the host event loop dispatches pointer events, so the channel could \
                 never be fed. Rebuild with `--target=wasm_browser --features wasm-threads` \
                 (design.md § Scheduler contract on WASM — Realization status)."
                    .to_string(),
            );
        }
        // Clone first — the surviving reference keeps the channel open across
        // every event (the local `tx` in `pointer_moves` drops at return).
        let ch = self.compile_expr(object)?.into_pointer_value();
        let clone_fn = self
            .module
            .get_function("karac_runtime_channel_clone")
            .expect("karac_runtime_channel_clone declared in Codegen::new");
        let cloned = self
            .builder
            .build_call(clone_fn, &[ch.into()], "ptr.chan.clone")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let i64_ty = self.context.i64_type();
        let ch_i64 = self
            .builder
            .build_ptr_to_int(cloned, i64_ty, "ptr.chan.i64")
            .unwrap();

        let host_fn = self.get_or_declare_pointer_moves_import();
        self.builder
            .build_call(host_fn, &[ch_i64.into()], "ptr.schedule")
            .unwrap();
        Ok(self.context.i64_type().const_zero().into())
    }

    /// Get-or-declare the `kara_host.__kara_pointer_moves(i64) -> ()` wasm
    /// import (compiler-emitted, sibling of `__kara_animation_frames`; the
    /// channel handle crosses the boundary as an `i64`). The element layout
    /// (`PointerEvent` = two `f64`s) is fixed and known to the glue, so it is
    /// not passed across — the glue hardcodes the marshalling + size.
    fn get_or_declare_pointer_moves_import(&self) -> inkwell::values::FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("__kara_pointer_moves") {
            return f;
        }
        use inkwell::attributes::AttributeLoc;
        let i64_ty = self.context.i64_type();
        let fn_ty = self.context.void_type().fn_type(&[i64_ty.into()], false);
        let f = self
            .module
            .add_function("__kara_pointer_moves", fn_ty, None);
        f.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("wasm-import-module", "kara_host"),
        );
        f.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("wasm-import-name", "__kara_pointer_moves"),
        );
        f
    }

    /// `tx.__schedule_wheel()` — the compiler builtin backing
    /// `std.web.events.wheel`. Identical shape to `__schedule_pointer_moves`
    /// (multi-shot, host owns the surviving cloned sender, non-unit payload);
    /// only the payload differs: the host marshals each `wheel` event as a
    /// `WheelEvent` ({ x, y, delta_x, delta_y }, four `f64`s = 32 bytes) into
    /// `karac_runtime_event_scratch()` and `channel_send`s it. Takes no
    /// argument. Same `--features wasm-threads` gate as the other host-async
    /// producers (a sequential build has no thread to park in `recv`).
    fn compile_channel_schedule_wheel(
        &mut self,
        object: &Expr,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if !args.is_empty() {
            return Err("Sender.__schedule_wheel expects no arguments".to_string());
        }
        if crate::target::active_target_is_wasm() && !crate::target::wasm_threads_enabled() {
            return Err(
                "std.web.events.wheel (host-async input stream) requires `--features \
                 wasm-threads` on this target: a sequential WASM build has no thread to block in \
                 `recv` while the host event loop dispatches wheel events, so the channel could \
                 never be fed. Rebuild with `--target=wasm_browser --features wasm-threads` \
                 (design.md § Scheduler contract on WASM — Realization status)."
                    .to_string(),
            );
        }
        // Clone first — the surviving reference keeps the channel open across
        // every event (the local `tx` in `wheel` drops at return).
        let ch = self.compile_expr(object)?.into_pointer_value();
        let clone_fn = self
            .module
            .get_function("karac_runtime_channel_clone")
            .expect("karac_runtime_channel_clone declared in Codegen::new");
        let cloned = self
            .builder
            .build_call(clone_fn, &[ch.into()], "wheel.chan.clone")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let i64_ty = self.context.i64_type();
        let ch_i64 = self
            .builder
            .build_ptr_to_int(cloned, i64_ty, "wheel.chan.i64")
            .unwrap();

        let host_fn = self.get_or_declare_wheel_import();
        self.builder
            .build_call(host_fn, &[ch_i64.into()], "wheel.schedule")
            .unwrap();
        Ok(self.context.i64_type().const_zero().into())
    }

    /// Get-or-declare the `kara_host.__kara_wheel(i64) -> ()` wasm import
    /// (compiler-emitted, sibling of `__kara_pointer_moves`; the channel handle
    /// crosses as an `i64`). The element layout (`WheelEvent` = four `f64`s) is
    /// fixed and known to the glue, so it is not passed across.
    fn get_or_declare_wheel_import(&self) -> inkwell::values::FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("__kara_wheel") {
            return f;
        }
        use inkwell::attributes::AttributeLoc;
        let i64_ty = self.context.i64_type();
        let fn_ty = self.context.void_type().fn_type(&[i64_ty.into()], false);
        let f = self.module.add_function("__kara_wheel", fn_ty, None);
        f.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("wasm-import-module", "kara_host"),
        );
        f.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("wasm-import-name", "__kara_wheel"),
        );
        f
    }

    /// `tx.__schedule_keydown()` — the compiler builtin backing
    /// `std.web.events.keydown`. Identical shape to `__schedule_wheel`
    /// (multi-shot, host owns the surviving cloned sender, non-unit payload);
    /// only the payload differs: the host marshals each `keydown` event as a
    /// `KeyEvent` ({ key_code }, one `i64` = 8 bytes) into
    /// `karac_runtime_event_scratch()` and `channel_send`s it. Takes no
    /// argument. Same `--features wasm-threads` gate as the other host-async
    /// producers (a sequential build has no thread to park in `recv`).
    fn compile_channel_schedule_keydown(
        &mut self,
        object: &Expr,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if !args.is_empty() {
            return Err("Sender.__schedule_keydown expects no arguments".to_string());
        }
        if crate::target::active_target_is_wasm() && !crate::target::wasm_threads_enabled() {
            return Err(
                "std.web.events.keydown (host-async input stream) requires `--features \
                 wasm-threads` on this target: a sequential WASM build has no thread to block in \
                 `recv` while the host event loop dispatches keydown events, so the channel could \
                 never be fed. Rebuild with `--target=wasm_browser --features wasm-threads` \
                 (design.md § Scheduler contract on WASM — Realization status)."
                    .to_string(),
            );
        }
        // Clone first — the surviving reference keeps the channel open across
        // every event (the local `tx` in `keydown` drops at return).
        let ch = self.compile_expr(object)?.into_pointer_value();
        let clone_fn = self
            .module
            .get_function("karac_runtime_channel_clone")
            .expect("karac_runtime_channel_clone declared in Codegen::new");
        let cloned = self
            .builder
            .build_call(clone_fn, &[ch.into()], "keydown.chan.clone")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let i64_ty = self.context.i64_type();
        let ch_i64 = self
            .builder
            .build_ptr_to_int(cloned, i64_ty, "keydown.chan.i64")
            .unwrap();

        let host_fn = self.get_or_declare_keydown_import();
        self.builder
            .build_call(host_fn, &[ch_i64.into()], "keydown.schedule")
            .unwrap();
        Ok(self.context.i64_type().const_zero().into())
    }

    /// Get-or-declare the `kara_host.__kara_keydown(i64) -> ()` wasm import
    /// (compiler-emitted, sibling of `__kara_wheel`; the channel handle crosses
    /// as an `i64`). The element layout (`KeyEvent` = one `i64`) is fixed and
    /// known to the glue, so it is not passed across.
    fn get_or_declare_keydown_import(&self) -> inkwell::values::FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("__kara_keydown") {
            return f;
        }
        use inkwell::attributes::AttributeLoc;
        let i64_ty = self.context.i64_type();
        let fn_ty = self.context.void_type().fn_type(&[i64_ty.into()], false);
        let f = self.module.add_function("__kara_keydown", fn_ty, None);
        f.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("wasm-import-module", "kara_host"),
        );
        f.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("wasm-import-name", "__kara_keydown"),
        );
        f
    }

    /// `tx.__schedule_keyup()` — the compiler builtin backing
    /// `std.web.events.keyup`, the key-release sibling of `__schedule_keydown`.
    /// Identical shape (multi-shot, host owns the surviving cloned sender, the
    /// same 8-byte `KeyEvent` payload); only the host event differs (`keyup`
    /// vs `keydown`). Takes no argument. Same `--features wasm-threads` gate as
    /// the other host-async producers.
    fn compile_channel_schedule_keyup(
        &mut self,
        object: &Expr,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if !args.is_empty() {
            return Err("Sender.__schedule_keyup expects no arguments".to_string());
        }
        if crate::target::active_target_is_wasm() && !crate::target::wasm_threads_enabled() {
            return Err(
                "std.web.events.keyup (host-async input stream) requires `--features \
                 wasm-threads` on this target: a sequential WASM build has no thread to block in \
                 `recv` while the host event loop dispatches keyup events, so the channel could \
                 never be fed. Rebuild with `--target=wasm_browser --features wasm-threads` \
                 (design.md § Scheduler contract on WASM — Realization status)."
                    .to_string(),
            );
        }
        // Clone first — the surviving reference keeps the channel open across
        // every event (the local `tx` in `keyup` drops at return).
        let ch = self.compile_expr(object)?.into_pointer_value();
        let clone_fn = self
            .module
            .get_function("karac_runtime_channel_clone")
            .expect("karac_runtime_channel_clone declared in Codegen::new");
        let cloned = self
            .builder
            .build_call(clone_fn, &[ch.into()], "keyup.chan.clone")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let i64_ty = self.context.i64_type();
        let ch_i64 = self
            .builder
            .build_ptr_to_int(cloned, i64_ty, "keyup.chan.i64")
            .unwrap();

        let host_fn = self.get_or_declare_keyup_import();
        self.builder
            .build_call(host_fn, &[ch_i64.into()], "keyup.schedule")
            .unwrap();
        Ok(self.context.i64_type().const_zero().into())
    }

    /// Get-or-declare the `kara_host.__kara_keyup(i64) -> ()` wasm import
    /// (compiler-emitted, sibling of `__kara_keydown`; the channel handle
    /// crosses as an `i64`). The element layout (`KeyEvent` = one `i64`) is
    /// fixed and known to the glue, so it is not passed across.
    fn get_or_declare_keyup_import(&self) -> inkwell::values::FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("__kara_keyup") {
            return f;
        }
        use inkwell::attributes::AttributeLoc;
        let i64_ty = self.context.i64_type();
        let fn_ty = self.context.void_type().fn_type(&[i64_ty.into()], false);
        let f = self.module.add_function("__kara_keyup", fn_ty, None);
        f.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("wasm-import-module", "kara_host"),
        );
        f.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("wasm-import-name", "__kara_keyup"),
        );
        f
    }

    /// `tx.__schedule_clicks()` — the compiler builtin backing
    /// `std.web.events.clicks`. Identical shape to `__schedule_pointer_moves`
    /// (multi-shot, host owns the surviving cloned sender); only the payload and
    /// host event differ: the host marshals each `click` event as a `ClickEvent`
    /// (two `f64`s — `x`, `y`; 16 bytes) into shared memory before
    /// `channel_send`. Takes no argument. Same `--features wasm-threads` gate as
    /// the other host-async producers.
    fn compile_channel_schedule_clicks(
        &mut self,
        object: &Expr,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if !args.is_empty() {
            return Err("Sender.__schedule_clicks expects no arguments".to_string());
        }
        if crate::target::active_target_is_wasm() && !crate::target::wasm_threads_enabled() {
            return Err(
                "std.web.events.clicks (host-async input stream) requires `--features \
                 wasm-threads` on this target: a sequential WASM build has no thread to block in \
                 `recv` while the host event loop dispatches click events, so the channel could \
                 never be fed. Rebuild with `--target=wasm_browser --features wasm-threads` \
                 (design.md § Scheduler contract on WASM — Realization status)."
                    .to_string(),
            );
        }
        // Clone first — the surviving reference keeps the channel open across
        // every event (the local `tx` in `clicks` drops at return).
        let ch = self.compile_expr(object)?.into_pointer_value();
        let clone_fn = self
            .module
            .get_function("karac_runtime_channel_clone")
            .expect("karac_runtime_channel_clone declared in Codegen::new");
        let cloned = self
            .builder
            .build_call(clone_fn, &[ch.into()], "clicks.chan.clone")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let i64_ty = self.context.i64_type();
        let ch_i64 = self
            .builder
            .build_ptr_to_int(cloned, i64_ty, "clicks.chan.i64")
            .unwrap();

        let host_fn = self.get_or_declare_clicks_import();
        self.builder
            .build_call(host_fn, &[ch_i64.into()], "clicks.schedule")
            .unwrap();
        Ok(self.context.i64_type().const_zero().into())
    }

    /// Get-or-declare the `kara_host.__kara_clicks(i64) -> ()` wasm import
    /// (compiler-emitted, sibling of `__kara_pointer_moves`; the channel handle
    /// crosses as an `i64`). The element layout (`ClickEvent` = two `f64`s) is
    /// fixed and known to the glue, so it is not passed across.
    fn get_or_declare_clicks_import(&self) -> inkwell::values::FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("__kara_clicks") {
            return f;
        }
        use inkwell::attributes::AttributeLoc;
        let i64_ty = self.context.i64_type();
        let fn_ty = self.context.void_type().fn_type(&[i64_ty.into()], false);
        let f = self.module.add_function("__kara_clicks", fn_ty, None);
        f.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("wasm-import-module", "kara_host"),
        );
        f.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("wasm-import-name", "__kara_clicks"),
        );
        f
    }

    /// `tx.__schedule_dblclick()` — the compiler builtin backing
    /// `std.web.events.dblclick`, the double-press sibling of
    /// `__schedule_clicks`. Identical shape and the same 16-byte `ClickEvent`
    /// payload (two `f64`s — `x`, `y`); only the host event differs (`dblclick`
    /// vs `click`). Takes no argument. Same `--features wasm-threads` gate as the
    /// other host-async producers.
    fn compile_channel_schedule_dblclick(
        &mut self,
        object: &Expr,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if !args.is_empty() {
            return Err("Sender.__schedule_dblclick expects no arguments".to_string());
        }
        if crate::target::active_target_is_wasm() && !crate::target::wasm_threads_enabled() {
            return Err(
                "std.web.events.dblclick (host-async input stream) requires `--features \
                 wasm-threads` on this target: a sequential WASM build has no thread to block in \
                 `recv` while the host event loop dispatches dblclick events, so the channel could \
                 never be fed. Rebuild with `--target=wasm_browser --features wasm-threads` \
                 (design.md § Scheduler contract on WASM — Realization status)."
                    .to_string(),
            );
        }
        // Clone first — the surviving reference keeps the channel open across
        // every event (the local `tx` in `dblclick` drops at return).
        let ch = self.compile_expr(object)?.into_pointer_value();
        let clone_fn = self
            .module
            .get_function("karac_runtime_channel_clone")
            .expect("karac_runtime_channel_clone declared in Codegen::new");
        let cloned = self
            .builder
            .build_call(clone_fn, &[ch.into()], "dblclick.chan.clone")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let i64_ty = self.context.i64_type();
        let ch_i64 = self
            .builder
            .build_ptr_to_int(cloned, i64_ty, "dblclick.chan.i64")
            .unwrap();

        let host_fn = self.get_or_declare_dblclick_import();
        self.builder
            .build_call(host_fn, &[ch_i64.into()], "dblclick.schedule")
            .unwrap();
        Ok(self.context.i64_type().const_zero().into())
    }

    /// Get-or-declare the `kara_host.__kara_dblclick(i64) -> ()` wasm import
    /// (compiler-emitted, sibling of `__kara_clicks`; the channel handle crosses
    /// as an `i64`). The element layout (`ClickEvent` = two `f64`s) is fixed and
    /// known to the glue, so it is not passed across.
    fn get_or_declare_dblclick_import(&self) -> inkwell::values::FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("__kara_dblclick") {
            return f;
        }
        use inkwell::attributes::AttributeLoc;
        let i64_ty = self.context.i64_type();
        let fn_ty = self.context.void_type().fn_type(&[i64_ty.into()], false);
        let f = self.module.add_function("__kara_dblclick", fn_ty, None);
        f.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("wasm-import-module", "kara_host"),
        );
        f.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("wasm-import-name", "__kara_dblclick"),
        );
        f
    }

    /// `tx.__schedule_resize()` — the compiler builtin backing
    /// `std.web.events.resize`. Identical shape to `__schedule_pointer_moves`
    /// (multi-shot, host owns the surviving cloned sender); only the payload and
    /// host event differ: the host marshals each `resize` event as a
    /// `ResizeEvent` (two `i64`s — `width`, `height`; 16 bytes) read off the
    /// window's current dimensions into shared memory before `channel_send`.
    /// Takes no argument. Same `--features wasm-threads` gate as the other
    /// host-async producers.
    fn compile_channel_schedule_resize(
        &mut self,
        object: &Expr,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if !args.is_empty() {
            return Err("Sender.__schedule_resize expects no arguments".to_string());
        }
        if crate::target::active_target_is_wasm() && !crate::target::wasm_threads_enabled() {
            return Err(
                "std.web.events.resize (host-async input stream) requires `--features \
                 wasm-threads` on this target: a sequential WASM build has no thread to block in \
                 `recv` while the host event loop dispatches resize events, so the channel could \
                 never be fed. Rebuild with `--target=wasm_browser --features wasm-threads` \
                 (design.md § Scheduler contract on WASM — Realization status)."
                    .to_string(),
            );
        }
        // Clone first — the surviving reference keeps the channel open across
        // every event (the local `tx` in `resize` drops at return).
        let ch = self.compile_expr(object)?.into_pointer_value();
        let clone_fn = self
            .module
            .get_function("karac_runtime_channel_clone")
            .expect("karac_runtime_channel_clone declared in Codegen::new");
        let cloned = self
            .builder
            .build_call(clone_fn, &[ch.into()], "resize.chan.clone")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let i64_ty = self.context.i64_type();
        let ch_i64 = self
            .builder
            .build_ptr_to_int(cloned, i64_ty, "resize.chan.i64")
            .unwrap();

        let host_fn = self.get_or_declare_resize_import();
        self.builder
            .build_call(host_fn, &[ch_i64.into()], "resize.schedule")
            .unwrap();
        Ok(self.context.i64_type().const_zero().into())
    }

    /// Get-or-declare the `kara_host.__kara_resize(i64) -> ()` wasm import
    /// (compiler-emitted, sibling of `__kara_clicks`; the channel handle crosses
    /// as an `i64`). The element layout (`ResizeEvent` = two `i64`s) is fixed and
    /// known to the glue, so it is not passed across.
    fn get_or_declare_resize_import(&self) -> inkwell::values::FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("__kara_resize") {
            return f;
        }
        use inkwell::attributes::AttributeLoc;
        let i64_ty = self.context.i64_type();
        let fn_ty = self.context.void_type().fn_type(&[i64_ty.into()], false);
        let f = self.module.add_function("__kara_resize", fn_ty, None);
        f.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("wasm-import-module", "kara_host"),
        );
        f.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("wasm-import-name", "__kara_resize"),
        );
        f
    }

    /// `tx.__schedule_contextmenu()` — the compiler builtin backing
    /// `std.web.events.contextmenu`, the right-click sibling of
    /// `__schedule_clicks`. Identical shape and the same 16-byte `ClickEvent`
    /// payload (two `f64`s — `x`, `y`); only the host event differs
    /// (`contextmenu` vs `click`, and the host listener preventDefaults the
    /// native menu). Takes no argument. Same `--features wasm-threads` gate as the
    /// other host-async producers.
    fn compile_channel_schedule_contextmenu(
        &mut self,
        object: &Expr,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if !args.is_empty() {
            return Err("Sender.__schedule_contextmenu expects no arguments".to_string());
        }
        if crate::target::active_target_is_wasm() && !crate::target::wasm_threads_enabled() {
            return Err(
                "std.web.events.contextmenu (host-async input stream) requires `--features \
                 wasm-threads` on this target: a sequential WASM build has no thread to block in \
                 `recv` while the host event loop dispatches contextmenu events, so the channel \
                 could never be fed. Rebuild with `--target=wasm_browser --features wasm-threads` \
                 (design.md § Scheduler contract on WASM — Realization status)."
                    .to_string(),
            );
        }
        // Clone first — the surviving reference keeps the channel open across
        // every event (the local `tx` in `contextmenu` drops at return).
        let ch = self.compile_expr(object)?.into_pointer_value();
        let clone_fn = self
            .module
            .get_function("karac_runtime_channel_clone")
            .expect("karac_runtime_channel_clone declared in Codegen::new");
        let cloned = self
            .builder
            .build_call(clone_fn, &[ch.into()], "contextmenu.chan.clone")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let i64_ty = self.context.i64_type();
        let ch_i64 = self
            .builder
            .build_ptr_to_int(cloned, i64_ty, "contextmenu.chan.i64")
            .unwrap();

        let host_fn = self.get_or_declare_contextmenu_import();
        self.builder
            .build_call(host_fn, &[ch_i64.into()], "contextmenu.schedule")
            .unwrap();
        Ok(self.context.i64_type().const_zero().into())
    }

    /// Get-or-declare the `kara_host.__kara_contextmenu(i64) -> ()` wasm import
    /// (compiler-emitted, sibling of `__kara_clicks`; the channel handle crosses
    /// as an `i64`). The element layout (`ClickEvent` = two `f64`s) is fixed and
    /// known to the glue, so it is not passed across.
    fn get_or_declare_contextmenu_import(&self) -> inkwell::values::FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("__kara_contextmenu") {
            return f;
        }
        use inkwell::attributes::AttributeLoc;
        let i64_ty = self.context.i64_type();
        let fn_ty = self.context.void_type().fn_type(&[i64_ty.into()], false);
        let f = self.module.add_function("__kara_contextmenu", fn_ty, None);
        f.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("wasm-import-module", "kara_host"),
        );
        f.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("wasm-import-name", "__kara_contextmenu"),
        );
        f
    }

    /// Get-or-declare the `kara_host.__kara_animation_frames(i64) -> ()` wasm
    /// import (compiler-emitted, sibling of `__kara_timer_after`; the channel
    /// handle crosses the boundary as an `i64`).
    fn get_or_declare_animation_frames_import(&self) -> inkwell::values::FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("__kara_animation_frames") {
            return f;
        }
        use inkwell::attributes::AttributeLoc;
        let i64_ty = self.context.i64_type();
        let fn_ty = self.context.void_type().fn_type(&[i64_ty.into()], false);
        let f = self
            .module
            .add_function("__kara_animation_frames", fn_ty, None);
        f.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("wasm-import-module", "kara_host"),
        );
        f.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("wasm-import-name", "__kara_animation_frames"),
        );
        f
    }

    /// Get-or-declare the `kara_host.__kara_timer_after(i64, i64) -> ()` wasm
    /// import. Compiler-emitted (no source `host fn` item), so the import
    /// attributes are attached here rather than via
    /// `declare_one_extern_function`. The browser/`none` C-ABI `kara_host`
    /// module is used unconditionally: `after` is `writes(Timer)` and Timer
    /// is provided only on `wasm_browser`, where `--features wasm-threads`
    /// (the only target that can run this) forbids `--bindings=component` —
    /// so the WIT host-package path is never reachable here.
    fn get_or_declare_timer_after_import(&self) -> inkwell::values::FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("__kara_timer_after") {
            return f;
        }
        use inkwell::attributes::AttributeLoc;
        let i64_ty = self.context.i64_type();
        let fn_ty = self
            .context
            .void_type()
            .fn_type(&[i64_ty.into(), i64_ty.into()], false);
        let f = self.module.add_function("__kara_timer_after", fn_ty, None);
        f.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("wasm-import-module", "kara_host"),
        );
        f.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("wasm-import-name", "__kara_timer_after"),
        );
        f
    }

    /// Get-or-declare the `kara_host.__kara_timer_every(i64, i64) -> ()` wasm
    /// import (compiler-emitted, sibling of `__kara_timer_after`; the channel
    /// handle crosses as an `i64`, the interval period as an `i64` ms).
    fn get_or_declare_timer_every_import(&self) -> inkwell::values::FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("__kara_timer_every") {
            return f;
        }
        use inkwell::attributes::AttributeLoc;
        let i64_ty = self.context.i64_type();
        let fn_ty = self
            .context
            .void_type()
            .fn_type(&[i64_ty.into(), i64_ty.into()], false);
        let f = self.module.add_function("__kara_timer_every", fn_ty, None);
        f.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("wasm-import-module", "kara_host"),
        );
        f.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("wasm-import-name", "__kara_timer_every"),
        );
        f
    }

    /// `tx.clone()` → a second handle to the same channel
    /// (`karac_runtime_channel_clone`: same pointer, refcount++). The new
    /// binding gets its own scope-exit `DropChannelEnd` like any channel end.
    fn compile_channel_clone(&mut self, object: &Expr) -> Result<BasicValueEnum<'ctx>, String> {
        let ch = self.compile_expr(object)?.into_pointer_value();
        let clone_fn = self
            .module
            .get_function("karac_runtime_channel_clone")
            .expect("karac_runtime_channel_clone declared in Codegen::new");
        let new = self
            .builder
            .build_call(clone_fn, &[ch.into()], "chan.clone")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();
        Ok(new)
    }
}
