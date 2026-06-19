//! Codegen for stdlib `TcpListener` / `TcpStream` (`runtime/stdlib/tcp.kara`).
//!
//! Four `#[compiler_builtin]` methods:
//!
//! - `TcpListener.bind(addr: String) -> TcpListener` — calls
//!   `karac_runtime_tcp_bind(addr_ptr, addr_len) -> i32`, then wraps
//!   the returned fd in a fresh `TcpListener { fd }` struct value.
//!   The runtime prints `BOUND_PORT=<n>` to stdout when the requested
//!   address ends in `:0` (ephemeral-port convention, matching
//!   `Server.serve_static`).
//!
//! - `TcpListener.accept(ref self) -> TcpStream` — Path A (per the
//!   Slice 6 design call): parks via `karac_park_on_fd(self.fd, 0u8)`
//!   so the yield happens at the kara state-machine level, then calls
//!   `karac_runtime_tcp_accept(self.fd)` for the raw `accept(2)` to
//!   pick up the now-readable connection. The returned i32 fd is
//!   wrapped in a fresh `TcpStream { fd }` struct value. The parking
//!   step composes through the same
//!   `emit_state_machine_invocation_for_park_on_fd` helper that the
//!   read / write methods (and future `WebSocket.recv` / `.send`)
//!   reuse.
//!
//! - `TcpStream.read(ref self, buf: mut Slice[u8]) -> Result[i64, TcpError]`
//!   parks on `self.fd` for read-readiness (`direction = 0u8`), then
//!   calls `karac_runtime_tcp_read(self.fd, buf.ptr, buf.len) -> i64`
//!   for the raw `read(2)`. The FFI returns the byte count on success
//!   (>= 0) or `-errno` on failure; `wrap_tcp_io_result` branches on
//!   the sign and packs the result into `Result.Ok(n)` or
//!   `Result.Err(TcpError.{Interrupted | Other(errno)})`.
//!
//! - `TcpStream.write(ref self, buf: Slice[u8]) -> Result[i64, TcpError]`
//!   parks on `self.fd` for write-readiness (`direction = 1u8`), then
//!   calls `karac_runtime_tcp_write(self.fd, buf.ptr, buf.len) -> i64`
//!   for the raw `write(2)`. Same Result wrapping as `read`.
//!
//! **karac_park_on_fd is emitted unconditionally** in every kara
//! binary (see `declarations.rs::synthesize_park_on_fd_layout`). The
//! synthesised state-struct + constructor + poll-fn carry the
//! canonical `{fd: i32, direction: u8}` shape — the same shape a
//! user-source `pub fn karac_park_on_fd(fd: i32, direction: u8) with
//! sends(Network) receives(Network) {}` declaration would produce,
//! so the unconditional emission is faithful to both the stdlib
//! lowering surface and the existing power-user surface (where a
//! user declares the primitive in their own source).

use inkwell::values::{BasicValue, BasicValueEnum};
use inkwell::{AddressSpace, IntPredicate};

use super::declarations::{KARAC_PARK_ON_FD, KARAC_PARK_ON_TIMER};

impl<'ctx> super::Codegen<'ctx> {
    /// Emit the state-machine invocation pattern for `karac_park_on_fd`
    /// inline at the current builder position: allocate the state struct
    /// via the constructor, store `fd` (field 1) and `direction` (field 2),
    /// invoke the poll-fn once (drives `state_0`: register + Pending), and
    /// on Pending block on the per-park completion slot until the
    /// dispatcher signals readiness — then deregister the fd and free the
    /// slot + state struct (async-sched slice 2/3). Specialised to the
    /// parking primitive's two-arg owned-param shape (no ref/slice
    /// handling needed).
    ///
    /// Used by stdlib `TcpListener.accept` (and future `TcpStream.read`
    /// / `.write` / `WebSocket.recv` / `.send`) codegen lowerings —
    /// when the kara source isn't visible to codegen (baked stdlib
    /// items don't reach `program.items`), the lowering can still
    /// compose with the leaf parking primitive through this helper.
    pub(super) fn emit_state_machine_invocation_for_park_on_fd(
        &mut self,
        fd: inkwell::values::IntValue<'ctx>,
        direction: inkwell::values::IntValue<'ctx>,
    ) {
        // A2 slice 2b.3: inside a coroutine-compiled network-boundary fn, the
        // park lowers to `register_fd(&frame.parked={shim,hdl}) + coro.suspend`
        // (dispatcher-driven), NOT the `park_slot_wait` thread-block below. On
        // return the builder is positioned at this park's resume edge, so the
        // caller's post-park syscall (`karac_runtime_tcp_read`/`accept`/`write`)
        // lands there verbatim — same separation the thread-block path uses.
        if let Some(ctx) = self.coro_ctx {
            self.emit_coro_park_suspend(fd, direction, &ctx);
            return;
        }
        let ctor_fn = self
            .state_machine_state_constructors
            .get(KARAC_PARK_ON_FD)
            .copied()
            .expect("karac_park_on_fd state-machine constructor must be emitted before codegen-side compose");
        let poll_fn = self
            .state_machine_poll_fns
            .get(KARAC_PARK_ON_FD)
            .copied()
            .expect("karac_park_on_fd poll-fn must be emitted before codegen-side compose");
        let state_struct = self
            .state_struct_types
            .get(KARAC_PARK_ON_FD)
            .copied()
            .expect(
                "karac_park_on_fd state-struct type must be emitted before codegen-side compose",
            );

        let cur_fn = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .expect("emit_state_machine_invocation_for_park_on_fd inside a function context");

        // Allocate state via the constructor.
        let state_call = self
            .builder
            .build_call(ctor_fn, &[], "kara.park.state")
            .expect("call karac_park_on_fd state constructor");
        let state_ptr = state_call
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        // Store fd at field 1, direction at field 2 (field 0 is the
        // i32 tag — already zero-initialised by the constructor's
        // calloc-equivalent).
        let fd_field_ptr = self
            .builder
            .build_struct_gep(state_struct, state_ptr, 1, "kara.park.fd.field")
            .expect("GEP fd field of karac_park_on_fd state struct");
        self.builder
            .build_store(fd_field_ptr, fd)
            .expect("store fd into state struct");
        let dir_field_ptr = self
            .builder
            .build_struct_gep(state_struct, state_ptr, 2, "kara.park.dir.field")
            .expect("GEP direction field of karac_park_on_fd state struct");
        self.builder
            .build_store(dir_field_ptr, direction)
            .expect("store direction into state struct");

        // Dispatcher-yield drive (async-sched slice 2/3). Invoke the
        // poll-fn ONCE — that runs `state_0` (allocate completion slot,
        // register fd, return Pending). On Pending we block on the slot
        // (`park_slot_wait`); the dispatcher thread, when the fd fires,
        // re-invokes the poll-fn at `state_1` which signals the slot. We do
        // NOT re-invoke the poll-fn ourselves — that would race the
        // dispatcher. After the wait returns we deregister the fd (so the
        // next park of the same fd gets a fresh registration rather than an
        // EEXIST error) and free the slot, then free the state struct.
        let wait_bb = self
            .context
            .append_basic_block(cur_fn, "kara.park.poll_wait");
        let done_bb = self
            .context
            .append_basic_block(cur_fn, "kara.park.poll_done");

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i8_ty = self.context.i8_type();
        let i64_ty = self.context.i64_type();
        let null_cancel = ptr_ty.const_null();
        let poll_call = self
            .builder
            .build_call(
                poll_fn,
                &[state_ptr.into(), null_cancel.into()],
                "kara.park.poll_result",
            )
            .expect("call karac_park_on_fd poll-fn");
        let poll_result = poll_call
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let is_pending = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                poll_result,
                i8_ty.const_int(0, false),
                "kara.park.is_pending",
            )
            .expect("icmp eq i8 poll_result, 0");
        // Pending → wait on the slot; Ready (defensive, state_0 always
        // yields Pending) → skip straight to cleanup.
        self.builder
            .build_conditional_branch(is_pending, wait_bb, done_bb)
            .expect("br on park poll discriminant");

        // wait_bb: block until the dispatcher signals readiness, then
        // deregister the fd and free the completion slot.
        self.builder.position_at_end(wait_bb);
        let slot_field_ptr = self
            .builder
            .build_struct_gep(state_struct, state_ptr, 6, "kara.park.slot.field")
            .expect("GEP completion-slot field of karac_park_on_fd state struct");
        let slot = self
            .builder
            .build_load(ptr_ty, slot_field_ptr, "kara.park.slot")
            .expect("load completion slot");
        let park_slot_wait_fn = self
            .module
            .get_function("karac_runtime_park_slot_wait")
            .expect("karac_runtime_park_slot_wait declared in Codegen::new");
        self.builder
            .build_call(park_slot_wait_fn, &[slot.into()], "")
            .expect("call karac_runtime_park_slot_wait");
        // One-shot deregister: load fd (field 1) + token (field 5), then
        // remove the registration so the fd can be re-parked later.
        let fd_reload_ptr = self
            .builder
            .build_struct_gep(state_struct, state_ptr, 1, "kara.park.fd.reload")
            .expect("GEP fd field for deregister");
        let fd_reload = self
            .builder
            .build_load(i64_ty, fd_reload_ptr, "kara.park.fd.val")
            .expect("load fd for deregister")
            .into_int_value();
        let token_ptr = self
            .builder
            .build_struct_gep(state_struct, state_ptr, 5, "kara.park.token.field")
            .expect("GEP registration-token field");
        let token = self
            .builder
            .build_load(i64_ty, token_ptr, "kara.park.token")
            .expect("load registration token")
            .into_int_value();
        let deregister_fn = self
            .module
            .get_function("karac_runtime_event_loop_deregister_fd")
            .expect("karac_runtime_event_loop_deregister_fd declared in Codegen::new");
        self.builder
            .build_call(
                deregister_fn,
                &[fd_reload.into(), token.into()],
                "kara.park.deregister",
            )
            .expect("call karac_runtime_event_loop_deregister_fd");
        let park_slot_free_fn = self
            .module
            .get_function("karac_runtime_park_slot_free")
            .expect("karac_runtime_park_slot_free declared in Codegen::new");
        self.builder
            .build_call(park_slot_free_fn, &[slot.into()], "")
            .expect("call karac_runtime_park_slot_free");
        self.builder
            .build_unconditional_branch(done_bb)
            .expect("br to park done after wait");

        self.builder.position_at_end(done_bb);
        self.builder
            .build_call(self.free_fn, &[state_ptr.into()], "")
            .expect("free karac_park_on_fd state struct");
    }

    /// Compose a `sleep_ms` call with the `karac_park_on_timer` primitive
    /// (phase-5 auto-par divergence A2a-2.2) — the timer-axis twin of
    /// `emit_state_machine_invocation_for_park_on_fd`. Allocate the state
    /// struct via its constructor, store `duration_nanos` (field 1), drive
    /// the poll-fn once (state_0 arms the deadline + returns Pending), block
    /// on the per-park completion slot until the dispatcher signals expiry,
    /// then free the slot and the state struct. No fd deregister: a timer
    /// has no fd / no `epoll_ctl` registration, and the dispatcher claims
    /// the heap registration itself.
    ///
    /// `sleep_ms` is non-resuming-with-value (returns unit), so this helper
    /// emits no terminal-value read. The builder is left positioned after
    /// the resume edge — a `sleep_ms(n)` statement's value is unit.
    pub(super) fn emit_state_machine_invocation_for_park_on_timer(
        &mut self,
        duration_nanos: inkwell::values::IntValue<'ctx>,
    ) {
        let ctor_fn = self
            .state_machine_state_constructors
            .get(KARAC_PARK_ON_TIMER)
            .copied()
            .expect("karac_park_on_timer constructor must be emitted before sleep_ms compose");
        let poll_fn = self
            .state_machine_poll_fns
            .get(KARAC_PARK_ON_TIMER)
            .copied()
            .expect("karac_park_on_timer poll-fn must be emitted before sleep_ms compose");
        let state_struct = self
            .state_struct_types
            .get(KARAC_PARK_ON_TIMER)
            .copied()
            .expect(
                "karac_park_on_timer state-struct type must be emitted before sleep_ms compose",
            );

        let cur_fn = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .expect("emit_state_machine_invocation_for_park_on_timer inside a function context");

        // Allocate state via the constructor (fills tag=0 + the parked
        // record); store the requested nap length at field 1.
        let state_ptr = self
            .builder
            .build_call(ctor_fn, &[], "kara.timer.state")
            .expect("call karac_park_on_timer state constructor")
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let dur_field_ptr = self
            .builder
            .build_struct_gep(state_struct, state_ptr, 1, "kara.timer.dur.field")
            .expect("GEP duration field of karac_park_on_timer state struct");
        self.builder
            .build_store(dur_field_ptr, duration_nanos)
            .expect("store duration into timer state struct");

        let wait_bb = self
            .context
            .append_basic_block(cur_fn, "kara.timer.poll_wait");
        let done_bb = self
            .context
            .append_basic_block(cur_fn, "kara.timer.poll_done");

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i8_ty = self.context.i8_type();
        let null_cancel = ptr_ty.const_null();
        // Drive the poll-fn ONCE — state_0 arms the deadline and returns
        // Pending. We do NOT re-invoke it; the dispatcher re-enters at
        // state_1 on expiry. On the (defensive) Ready path skip to cleanup.
        let poll_result = self
            .builder
            .build_call(
                poll_fn,
                &[state_ptr.into(), null_cancel.into()],
                "kara.timer.poll_result",
            )
            .expect("call karac_park_on_timer poll-fn")
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let is_pending = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                poll_result,
                i8_ty.const_int(0, false),
                "kara.timer.is_pending",
            )
            .expect("icmp eq i8 poll_result, 0");
        self.builder
            .build_conditional_branch(is_pending, wait_bb, done_bb)
            .expect("br on timer poll discriminant");

        // wait_bb: block on the completion slot until the dispatcher signals
        // expiry, then free the slot. No fd deregister (no fd / no epoll).
        self.builder.position_at_end(wait_bb);
        let slot_field_ptr = self
            .builder
            .build_struct_gep(state_struct, state_ptr, 4, "kara.timer.slot.field")
            .expect("GEP completion-slot field of karac_park_on_timer state struct");
        let slot = self
            .builder
            .build_load(ptr_ty, slot_field_ptr, "kara.timer.slot")
            .expect("load timer completion slot");
        let park_slot_wait_fn = self
            .module
            .get_function("karac_runtime_park_slot_wait")
            .expect("karac_runtime_park_slot_wait declared in Codegen::new");
        self.builder
            .build_call(park_slot_wait_fn, &[slot.into()], "")
            .expect("call karac_runtime_park_slot_wait");
        let park_slot_free_fn = self
            .module
            .get_function("karac_runtime_park_slot_free")
            .expect("karac_runtime_park_slot_free declared in Codegen::new");
        self.builder
            .build_call(park_slot_free_fn, &[slot.into()], "")
            .expect("call karac_runtime_park_slot_free");
        self.builder
            .build_unconditional_branch(done_bb)
            .expect("br to timer done after wait");

        self.builder.position_at_end(done_bb);
        self.builder
            .build_call(self.free_fn, &[state_ptr.into()], "")
            .expect("free karac_park_on_timer state struct");
    }

    /// Lower `TcpListener.bind(addr: String) -> TcpListener` to a call
    /// into `karac_runtime_tcp_bind(addr_ptr, addr_len) -> i32`, then
    /// pack the returned fd into a fresh `TcpListener { fd }` struct
    /// value. Returns the struct value; the assoc_call.rs dispatch
    /// site forwards it as the call expression's result.
    pub(super) fn lower_tcp_listener_bind(
        &mut self,
        addr_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Kāra `String` is the same `{ptr, len, cap}` shape as `Vec[u8]`
        // — extract `ptr` (field 0) and `len` (field 1). Cap is
        // ignored at the FFI boundary.
        let addr_sv = addr_val.into_struct_value();
        let addr_ptr = self
            .builder
            .build_extract_value(addr_sv, 0, "tcp.bind.addr.ptr")
            .unwrap()
            .into_pointer_value();
        let addr_len = self
            .builder
            .build_extract_value(addr_sv, 1, "tcp.bind.addr.len")
            .unwrap()
            .into_int_value();

        let bind_fn = self
            .module
            .get_function("karac_runtime_tcp_bind")
            .expect("karac_runtime_tcp_bind declared in Codegen::new");
        let fd_call = self
            .builder
            .build_call(bind_fn, &[addr_ptr.into(), addr_len.into()], "tcp.bind.fd")
            .expect("call karac_runtime_tcp_bind");
        let fd = fd_call.try_as_basic_value().unwrap_basic().into_int_value();

        // Pack into `Result[TcpListener, TcpError]` (phase-8 line 64 audit):
        // `Ok(TcpListener { fd })` on `fd >= 0`, else `Err(TcpError.Other(-1))`.
        self.build_fd_construct_result(fd, "TcpError", "Other", "tcp.bind")
    }

    /// Lower `TcpStream.connect(addr: String) -> Result[TcpStream,
    /// TcpError]` — the plain-TCP client mirror of `bind`. Extract the
    /// addr `String`'s `{ptr, len}`, call `karac_runtime_tcp_connect`
    /// (a blocking `connect(2)`, no parking), then wrap the returned fd
    /// in `Result[TcpStream, TcpError]` via the shared
    /// `build_fd_construct_result` (`Ok(TcpStream { fd })` on `fd >= 0`,
    /// else `Err(TcpError.Other(-1))`).
    pub(super) fn lower_tcp_stream_connect(
        &mut self,
        addr_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let addr_sv = addr_val.into_struct_value();
        let addr_ptr = self
            .builder
            .build_extract_value(addr_sv, 0, "tcp.connect.addr.ptr")
            .unwrap()
            .into_pointer_value();
        let addr_len = self
            .builder
            .build_extract_value(addr_sv, 1, "tcp.connect.addr.len")
            .unwrap()
            .into_int_value();

        let connect_fn = self
            .module
            .get_function("karac_runtime_tcp_connect")
            .expect("karac_runtime_tcp_connect declared in Codegen::new");
        let fd_call = self
            .builder
            .build_call(
                connect_fn,
                &[addr_ptr.into(), addr_len.into()],
                "tcp.connect.fd",
            )
            .expect("call karac_runtime_tcp_connect");
        let fd = fd_call.try_as_basic_value().unwrap_basic().into_int_value();

        self.build_fd_construct_result(fd, "TcpError", "Other", "tcp.connect")
    }

    /// Lower `TcpStream.try_clone(ref self) -> Result[TcpStream, TcpError]`
    /// to: extract `self.fd`, call `karac_runtime_tcp_try_clone(self.fd)`
    /// (a `dup(2)`, no parking), then wrap the returned fd in
    /// `Result[TcpStream, TcpError]` via the shared
    /// `build_fd_construct_result` (`Ok(TcpStream { fd })` on `fd >= 0`,
    /// else `Err(TcpError.Other(-1))`). The dup'd handle is an independent
    /// owned `TcpStream` over the same socket — used to split a connection
    /// into a read-half and a write-half for a full-duplex splice.
    pub(super) fn lower_tcp_stream_try_clone(
        &mut self,
        self_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fd = self.extract_fd_from_tcp_struct(self_val, "tcp.try_clone.self.fd");
        let clone_fn = self
            .module
            .get_function("karac_runtime_tcp_try_clone")
            .expect("karac_runtime_tcp_try_clone declared in Codegen::new");
        let new_fd_call = self
            .builder
            .build_call(clone_fn, &[fd.into()], "tcp.try_clone.fd")
            .expect("call karac_runtime_tcp_try_clone");
        let new_fd = new_fd_call
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        self.build_fd_construct_result(new_fd, "TcpError", "Other", "tcp.try_clone")
    }

    /// Lower `TcpStream.shutdown_write(ref self) -> Result[Unit, TcpError]`
    /// to: extract `self.fd`, call `karac_runtime_tcp_shutdown(self.fd, 1)`
    /// (1 = `SHUT_WR`, no parking), then build a `Result[Unit, TcpError]`
    /// from the 0/-1 status. The write half-close sends a FIN so a proxy can
    /// propagate one direction's EOF across a full-duplex splice.
    pub(super) fn lower_tcp_stream_shutdown_write(
        &mut self,
        self_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fd = self.extract_fd_from_tcp_struct(self_val, "tcp.shutwr.self.fd");
        let how = self.context.i64_type().const_int(1, false); // 1 = Write
        let shutdown_fn = self
            .module
            .get_function("karac_runtime_tcp_shutdown")
            .expect("karac_runtime_tcp_shutdown declared in Codegen::new");
        let status_call = self
            .builder
            .build_call(shutdown_fn, &[fd.into(), how.into()], "tcp.shutwr.status")
            .expect("call karac_runtime_tcp_shutdown");
        let status_i32 = status_call
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        // The FFI returns i32; widen to i64 for the shared status-result build.
        let status = self
            .builder
            .build_int_s_extend(status_i32, self.context.i64_type(), "tcp.shutwr.status.i64")
            .unwrap();
        self.build_unit_status_result(status, "tcp.shutwr")
    }

    /// Build a `Result[Unit, TcpError]` from an i64 status code: `>= 0`
    /// produces `Result.Ok(Unit)`, `< 0` produces `Result.Err(TcpError.
    /// Other(-1))`. Mirrors `lower_tcp_stream_write_all`'s inline
    /// Result-construction (the `{tag, w0, w1}` tagged-union shape via
    /// `enum_layouts`) for the no-payload-success case.
    pub(super) fn build_unit_status_result(
        &mut self,
        status: inkwell::values::IntValue<'ctx>,
        label_prefix: &str,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let ctx = self.context;
        let i64_ty = ctx.i64_type();
        let zero = i64_ty.const_zero();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "build_unit_status_result outside fn".to_string())?;

        let ok_bb = ctx.append_basic_block(fn_val, &format!("{label_prefix}.ok"));
        let err_bb = ctx.append_basic_block(fn_val, &format!("{label_prefix}.err"));
        let cont_bb = ctx.append_basic_block(fn_val, &format!("{label_prefix}.cont"));
        let is_ok = self
            .builder
            .build_int_compare(
                IntPredicate::SGE,
                status,
                zero,
                &format!("{label_prefix}.is_ok"),
            )
            .unwrap();
        self.builder
            .build_conditional_branch(is_ok, ok_bb, err_bb)
            .unwrap();

        let result_layout = self
            .enum_layouts
            .get("Result")
            .expect("Result layout seeded");
        let result_ty = result_layout.llvm_type;
        let ok_tag = *result_layout.tags.get("Ok").expect("Result.Ok tag seeded");
        let err_tag = *result_layout
            .tags
            .get("Err")
            .expect("Result.Err tag seeded");
        let tcp_err_layout = self
            .enum_layouts
            .get("TcpError")
            .expect("TcpError layout seeded");
        let other_tag = *tcp_err_layout
            .tags
            .get("Other")
            .expect("TcpError.Other tag seeded");

        // ── ok: Result.Ok(Unit) = {ok_tag, 0, 0}
        self.builder.position_at_end(ok_bb);
        let mut ok_agg = result_ty.get_undef();
        ok_agg = self
            .builder
            .build_insert_value(
                ok_agg,
                i64_ty.const_int(ok_tag, false),
                0,
                &format!("{label_prefix}.ok.tag"),
            )
            .unwrap()
            .into_struct_value();
        ok_agg = self
            .builder
            .build_insert_value(ok_agg, zero, 1, &format!("{label_prefix}.ok.w0"))
            .unwrap()
            .into_struct_value();
        ok_agg = self
            .builder
            .build_insert_value(ok_agg, zero, 2, &format!("{label_prefix}.ok.w1"))
            .unwrap()
            .into_struct_value();
        let ok_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        // ── err: Result.Err(TcpError.Other(-1)) = {err_tag, other_tag, -1}
        self.builder.position_at_end(err_bb);
        let neg_one = i64_ty.const_int((-1i64) as u64, false);
        let mut err_agg = result_ty.get_undef();
        err_agg = self
            .builder
            .build_insert_value(
                err_agg,
                i64_ty.const_int(err_tag, false),
                0,
                &format!("{label_prefix}.err.tag"),
            )
            .unwrap()
            .into_struct_value();
        err_agg = self
            .builder
            .build_insert_value(
                err_agg,
                i64_ty.const_int(other_tag, false),
                1,
                &format!("{label_prefix}.err.w0"),
            )
            .unwrap()
            .into_struct_value();
        err_agg = self
            .builder
            .build_insert_value(err_agg, neg_one, 2, &format!("{label_prefix}.err.w1"))
            .unwrap()
            .into_struct_value();
        let err_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        // ── cont: phi between the two arms.
        self.builder.position_at_end(cont_bb);
        let phi = self
            .builder
            .build_phi(result_ty, &format!("{label_prefix}.result"))
            .unwrap();
        phi.add_incoming(&[
            (&ok_agg.as_basic_value_enum(), ok_end),
            (&err_agg.as_basic_value_enum(), err_end),
        ]);
        Ok(phi.as_basic_value())
    }

    /// Lower `TcpListener.accept(ref self) -> TcpStream` to: park on
    /// the listener's fd (via `karac_park_on_fd(self.fd, 0u8)`), call
    /// `karac_runtime_tcp_accept(self.fd)` for the raw accept(2), then
    /// wrap the returned i32 fd in a `TcpStream { fd }` struct value.
    /// On accept failure the FFI returns -1, surfacing as
    /// `TcpStream { fd: -1 }`.
    pub(super) fn lower_tcp_listener_accept(
        &mut self,
        self_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fd = self.extract_fd_from_tcp_struct(self_val, "tcp.accept.self.fd");

        // Park on the listener's fd for readability (direction = 0
        // = Read). This is the kara-level state-machine yield point;
        // the slice 6/8 machinery emits the constructor + poll-loop
        // inline so the parent function (if itself a network-
        // boundary function) can compose its own state machine
        // through this yield.
        let direction = self.context.i8_type().const_int(0, false);
        self.emit_state_machine_invocation_for_park_on_fd(fd, direction);

        // Now do the raw accept(2). At this point the listener is
        // known readable (the park returned Ready), so accept should
        // succeed without blocking; the runtime FFI returns -1 only
        // on catastrophic failure or `EAGAIN` (which signals a
        // missed-wakeup bug, not a normal case).
        let accept_fn = self
            .module
            .get_function("karac_runtime_tcp_accept")
            .expect("karac_runtime_tcp_accept declared in Codegen::new");
        let accept_call = self
            .builder
            .build_call(accept_fn, &[fd.into()], "tcp.accept.conn_fd")
            .expect("call karac_runtime_tcp_accept");
        let conn_fd = accept_call
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();

        // Pack into `Result[TcpStream, TcpError]` (phase-8 line 64 audit):
        // `Ok(TcpStream { fd })` on `fd >= 0`, else `Err(TcpError.Other(-1))`.
        self.build_fd_construct_result(conn_fd, "TcpError", "Other", "tcp.accept")
    }

    /// Lower `TcpStream.read(ref self, buf: mut Slice[u8]) -> i64` to:
    /// park on `self.fd` for read-readiness, then call
    /// `karac_runtime_tcp_read(self.fd, buf.ptr, buf.len)` for the raw
    /// `read(2)`. Returns the byte count read (-1 on error, 0 on EOF).
    pub(super) fn lower_tcp_stream_read(
        &mut self,
        self_val: BasicValueEnum<'ctx>,
        buf_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        self.lower_tcp_stream_io(self_val, buf_val, /*is_write=*/ false)
    }

    /// Lower `TcpStream.write(ref self, buf: Slice[u8]) -> i64` to:
    /// park on `self.fd` for write-readiness, then call
    /// `karac_runtime_tcp_write(self.fd, buf.ptr, buf.len)` for the
    /// raw `write(2)`. Returns the byte count written (-1 on error).
    pub(super) fn lower_tcp_stream_write(
        &mut self,
        self_val: BasicValueEnum<'ctx>,
        buf_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        self.lower_tcp_stream_io(self_val, buf_val, /*is_write=*/ true)
    }

    /// Lower `TcpStream.write_all(ref self, buf: Slice[u8]) ->
    /// Result[i64, TcpError]` — loop calling the raw write FFI until
    /// all `buf.len()` bytes have been pushed, absorbing partial
    /// writes and retrying on EINTR. Returns `Ok(buf.len())` on
    /// success or `Err(TcpError.Other(errno))` on the first
    /// unrecoverable error.
    ///
    /// This lowering exists as `#[compiler_builtin]` rather than
    /// pure-kara stdlib source because codegen only compiles user
    /// `program.items` — stdlib non-`#[compiler_builtin]` method
    /// bodies don't reach codegen (same gap that forces explicit
    /// layout seeds in `seed_builtin_enum_layouts` for stdlib
    /// enums). When that gap closes, this lowering can be replaced
    /// with the pure-kara while-loop body sketched in
    /// `runtime/stdlib/tcp.kara::TcpStream.write_all`.
    pub(super) fn lower_tcp_stream_write_all(
        &mut self,
        self_val: BasicValueEnum<'ctx>,
        buf_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let ctx = self.context;
        let i64_ty = ctx.i64_type();
        let i8_ty = ctx.i8_type();
        let zero_i64 = i64_ty.const_zero();

        let fd = self.extract_fd_from_tcp_struct(self_val, "tcp.wa.self.fd");

        let buf_sv = buf_val.into_struct_value();
        let buf_ptr = self
            .builder
            .build_extract_value(buf_sv, 0, "tcp.wa.buf.ptr")
            .unwrap()
            .into_pointer_value();
        let buf_len = self
            .builder
            .build_extract_value(buf_sv, 1, "tcp.wa.buf.len")
            .unwrap()
            .into_int_value();

        let fn_val = self
            .current_fn
            .ok_or_else(|| "TcpStream.write_all lowered outside fn".to_string())?;

        let written_slot = self.create_entry_alloca(fn_val, "tcp.wa.written", i64_ty.into());
        self.builder.build_store(written_slot, zero_i64).unwrap();

        let loop_head = ctx.append_basic_block(fn_val, "tcp.wa.loop.head");
        let loop_body = ctx.append_basic_block(fn_val, "tcp.wa.loop.body");
        let advance = ctx.append_basic_block(fn_val, "tcp.wa.advance");
        let err_check = ctx.append_basic_block(fn_val, "tcp.wa.err.check");
        let err_exit = ctx.append_basic_block(fn_val, "tcp.wa.err.exit");
        let ok_exit = ctx.append_basic_block(fn_val, "tcp.wa.ok.exit");
        let cont = ctx.append_basic_block(fn_val, "tcp.wa.cont");

        self.builder.build_unconditional_branch(loop_head).unwrap();

        // ── loop_head: if written >= buf.len, exit Ok; else body.
        self.builder.position_at_end(loop_head);
        let written = self
            .builder
            .build_load(i64_ty, written_slot, "tcp.wa.written.load")
            .unwrap()
            .into_int_value();
        let is_done = self
            .builder
            .build_int_compare(IntPredicate::SGE, written, buf_len, "tcp.wa.is_done")
            .unwrap();
        self.builder
            .build_conditional_branch(is_done, ok_exit, loop_body)
            .unwrap();

        // ── loop_body: chunk_ptr = buf.ptr + written, remaining = buf.len - written,
        //    park on write-readiness, call FFI, branch on success.
        self.builder.position_at_end(loop_body);
        let remaining = self
            .builder
            .build_int_sub(buf_len, written, "tcp.wa.remaining")
            .unwrap();
        let chunk_ptr = unsafe {
            self.builder
                .build_in_bounds_gep(i8_ty, buf_ptr, &[written], "tcp.wa.chunk.ptr")
                .unwrap()
        };
        let dir_write = i8_ty.const_int(1, false);
        self.emit_state_machine_invocation_for_park_on_fd(fd, dir_write);

        let write_fn = self
            .module
            .get_function("karac_runtime_tcp_write")
            .expect("karac_runtime_tcp_write declared in Codegen::new");
        let write_call = self
            .builder
            .build_call(
                write_fn,
                &[fd.into(), chunk_ptr.into(), remaining.into()],
                "tcp.wa.write.n",
            )
            .expect("call karac_runtime_tcp_write");
        let n = write_call
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let is_ok = self
            .builder
            .build_int_compare(IntPredicate::SGE, n, zero_i64, "tcp.wa.is_ok")
            .unwrap();
        self.builder
            .build_conditional_branch(is_ok, advance, err_check)
            .unwrap();

        // ── advance: written += n, br loop_head.
        self.builder.position_at_end(advance);
        let new_written = self
            .builder
            .build_int_add(written, n, "tcp.wa.new_written")
            .unwrap();
        self.builder.build_store(written_slot, new_written).unwrap();
        self.builder.build_unconditional_branch(loop_head).unwrap();

        // ── err_check: errno = -n; EINTR retries (back to loop_head, no advance),
        //    everything else exits with Err.
        self.builder.position_at_end(err_check);
        let errno = self
            .builder
            .build_int_sub(zero_i64, n, "tcp.wa.errno")
            .unwrap();
        let eintr = i64_ty.const_int(4, false);
        let is_eintr = self
            .builder
            .build_int_compare(IntPredicate::EQ, errno, eintr, "tcp.wa.is_eintr")
            .unwrap();
        self.builder
            .build_conditional_branch(is_eintr, loop_head, err_exit)
            .unwrap();
        let err_check_end_bb = self.builder.get_insert_block().unwrap();

        // ── err_exit: build Result.Err(TcpError.Other(errno)), br cont.
        self.builder.position_at_end(err_exit);
        let errno_phi = self.builder.build_phi(i64_ty, "tcp.wa.errno.phi").unwrap();
        errno_phi.add_incoming(&[(&errno.as_basic_value_enum(), err_check_end_bb)]);
        let result_layout = self
            .enum_layouts
            .get("Result")
            .expect("Result layout seeded");
        let result_ty = result_layout.llvm_type;
        let err_tag = *result_layout
            .tags
            .get("Err")
            .expect("Result.Err tag seeded");
        let ok_tag = *result_layout.tags.get("Ok").expect("Result.Ok tag seeded");
        let tcp_err_layout = self
            .enum_layouts
            .get("TcpError")
            .expect("TcpError layout seeded");
        let other_tag = *tcp_err_layout
            .tags
            .get("Other")
            .expect("TcpError.Other tag seeded");
        let mut err_agg = result_ty.get_undef();
        err_agg = self
            .builder
            .build_insert_value(
                err_agg,
                i64_ty.const_int(err_tag, false),
                0,
                "tcp.wa.err.tag",
            )
            .unwrap()
            .into_struct_value();
        err_agg = self
            .builder
            .build_insert_value(
                err_agg,
                i64_ty.const_int(other_tag, false),
                1,
                "tcp.wa.err.tcp_err.w0",
            )
            .unwrap()
            .into_struct_value();
        let errno_phi_val = errno_phi.as_basic_value().into_int_value();
        err_agg = self
            .builder
            .build_insert_value(err_agg, errno_phi_val, 2, "tcp.wa.err.tcp_err.w1")
            .unwrap()
            .into_struct_value();
        let err_exit_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(cont).unwrap();

        // ── ok_exit: build Result.Ok(written_final), br cont.
        self.builder.position_at_end(ok_exit);
        let written_final = self
            .builder
            .build_load(i64_ty, written_slot, "tcp.wa.written.final")
            .unwrap()
            .into_int_value();
        let mut ok_agg = result_ty.get_undef();
        ok_agg = self
            .builder
            .build_insert_value(ok_agg, i64_ty.const_int(ok_tag, false), 0, "tcp.wa.ok.tag")
            .unwrap()
            .into_struct_value();
        ok_agg = self
            .builder
            .build_insert_value(ok_agg, written_final, 1, "tcp.wa.ok.n")
            .unwrap()
            .into_struct_value();
        let ok_exit_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(cont).unwrap();

        // ── cont: phi between Err and Ok arms.
        self.builder.position_at_end(cont);
        let phi = self.builder.build_phi(result_ty, "tcp.wa.result").unwrap();
        phi.add_incoming(&[
            (&ok_agg.as_basic_value_enum(), ok_exit_end_bb),
            (&err_agg.as_basic_value_enum(), err_exit_end_bb),
        ]);
        Ok(phi.as_basic_value())
    }

    /// Shared lowering for `TcpStream.read` / `.write`: extract
    /// self.fd, extract buf.{ptr, len} (Slice's 2-word `{ptr, i64}`
    /// layout), park on the appropriate direction, then call the
    /// corresponding raw-syscall FFI.
    fn lower_tcp_stream_io(
        &mut self,
        self_val: BasicValueEnum<'ctx>,
        buf_val: BasicValueEnum<'ctx>,
        is_write: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fd = self.extract_fd_from_tcp_struct(self_val, "tcp.io.self.fd");

        // Slice[u8] layout matches `slice_struct_type()` in
        // `types_lowering.rs`: `{ ptr data, i64 len }`. The bytes/len
        // we hand to the FFI are just the two fields. `mut Slice` has
        // the same physical layout — mutability is a type-system
        // concept only.
        let buf_sv = buf_val.into_struct_value();
        let buf_ptr = self
            .builder
            .build_extract_value(buf_sv, 0, "tcp.io.buf.ptr")
            .unwrap()
            .into_pointer_value();
        let buf_len = self
            .builder
            .build_extract_value(buf_sv, 1, "tcp.io.buf.len")
            .unwrap()
            .into_int_value();

        // Park on the stream fd for the right direction.
        let direction = self
            .context
            .i8_type()
            .const_int(if is_write { 1 } else { 0 }, false);
        self.emit_state_machine_invocation_for_park_on_fd(fd, direction);

        // Raw syscall.
        let fn_name = if is_write {
            "karac_runtime_tcp_write"
        } else {
            "karac_runtime_tcp_read"
        };
        let io_fn = self
            .module
            .get_function(fn_name)
            .unwrap_or_else(|| panic!("{fn_name} declared in Codegen::new"));
        let io_call = self
            .builder
            .build_call(
                io_fn,
                &[fd.into(), buf_ptr.into(), buf_len.into()],
                if is_write {
                    "tcp.write.n"
                } else {
                    "tcp.read.n"
                },
            )
            .unwrap_or_else(|_| panic!("call {fn_name}"));
        let n = io_call.try_as_basic_value().unwrap_basic().into_int_value();
        self.wrap_tcp_io_result(n, is_write)
    }

    /// Wrap an `i64 n` (the raw return from `karac_runtime_tcp_read /
    /// _tcp_write`) in `Result[i64, TcpError]`. The runtime FFIs
    /// return the byte count (>= 0) on success and `-errno` on
    /// syscall failure; this lowering branches on the sign, builds
    /// the matching variant, and phi-merges.
    ///
    /// Error classification uses errno=4 (EINTR; POSIX-standardised
    /// across Linux/macOS/BSD/Solaris) — that single value picks the
    /// `TcpError.Interrupted` variant, everything else lands in
    /// `TcpError.Other(errno)`. The classification is a `select` pair
    /// (no extra basic blocks) since the constructed TcpError value
    /// is only used in the Err arm anyway.
    pub(super) fn wrap_tcp_io_result(
        &mut self,
        n: inkwell::values::IntValue<'ctx>,
        is_write: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let ctx = self.context;
        let i64_ty = ctx.i64_type();
        let label_prefix = if is_write { "tcp.write" } else { "tcp.read" };

        let result_layout = self
            .enum_layouts
            .get("Result")
            .expect("Result layout seeded by seed_builtin_enum_layouts");
        let result_ty = result_layout.llvm_type;
        let ok_tag = *result_layout.tags.get("Ok").expect("Result.Ok tag seeded");
        let err_tag = *result_layout
            .tags
            .get("Err")
            .expect("Result.Err tag seeded");

        let tcp_err_layout = self
            .enum_layouts
            .get("TcpError")
            .expect("TcpError layout seeded by seed_builtin_enum_layouts");
        let interrupted_tag = *tcp_err_layout
            .tags
            .get("Interrupted")
            .expect("TcpError.Interrupted tag seeded");
        let other_tag = *tcp_err_layout
            .tags
            .get("Other")
            .expect("TcpError.Other tag seeded");

        let fn_val = self
            .current_fn
            .ok_or_else(|| "tcp io Result wrapping outside fn".to_string())?;
        let ok_bb = ctx.append_basic_block(fn_val, &format!("{label_prefix}.ok"));
        let err_bb = ctx.append_basic_block(fn_val, &format!("{label_prefix}.err"));
        let cont_bb = ctx.append_basic_block(fn_val, &format!("{label_prefix}.cont"));

        let zero_i64 = i64_ty.const_zero();
        let is_success = self
            .builder
            .build_int_compare(
                IntPredicate::SGE,
                n,
                zero_i64,
                &format!("{label_prefix}.is_ok"),
            )
            .unwrap();
        self.builder
            .build_conditional_branch(is_success, ok_bb, err_bb)
            .unwrap();

        // ── Ok arm: Result.Ok(n) — tag at field 0, i64 payload at field 1.
        self.builder.position_at_end(ok_bb);
        let mut ok_agg = result_ty.get_undef();
        ok_agg = self
            .builder
            .build_insert_value(
                ok_agg,
                i64_ty.const_int(ok_tag, false),
                0,
                &format!("{label_prefix}.ok.tag"),
            )
            .unwrap()
            .into_struct_value();
        ok_agg = self
            .builder
            .build_insert_value(ok_agg, n, 1, &format!("{label_prefix}.ok.n"))
            .unwrap()
            .into_struct_value();
        let ok_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        // ── Err arm: classify errno, build TcpError, wrap in Result.Err.
        // TcpError occupies 2 words {tag, payload_word}; both go into
        // Result's fields 1 and 2.
        self.builder.position_at_end(err_bb);
        let errno = self
            .builder
            .build_int_sub(zero_i64, n, &format!("{label_prefix}.errno"))
            .unwrap();
        let eintr = i64_ty.const_int(4, false);
        let is_eintr = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                errno,
                eintr,
                &format!("{label_prefix}.is_eintr"),
            )
            .unwrap();
        let tcp_err_word_0 = self
            .builder
            .build_select(
                is_eintr,
                i64_ty.const_int(interrupted_tag, false),
                i64_ty.const_int(other_tag, false),
                &format!("{label_prefix}.tcp_err.tag"),
            )
            .unwrap()
            .into_int_value();
        let tcp_err_word_1 = self
            .builder
            .build_select(
                is_eintr,
                zero_i64,
                errno,
                &format!("{label_prefix}.tcp_err.errno"),
            )
            .unwrap()
            .into_int_value();
        let mut err_agg = result_ty.get_undef();
        err_agg = self
            .builder
            .build_insert_value(
                err_agg,
                i64_ty.const_int(err_tag, false),
                0,
                &format!("{label_prefix}.err.tag"),
            )
            .unwrap()
            .into_struct_value();
        err_agg = self
            .builder
            .build_insert_value(
                err_agg,
                tcp_err_word_0,
                1,
                &format!("{label_prefix}.err.tcp_err.w0"),
            )
            .unwrap()
            .into_struct_value();
        err_agg = self
            .builder
            .build_insert_value(
                err_agg,
                tcp_err_word_1,
                2,
                &format!("{label_prefix}.err.tcp_err.w1"),
            )
            .unwrap()
            .into_struct_value();
        let err_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        // ── Continuation: phi between Ok and Err arms.
        self.builder.position_at_end(cont_bb);
        let phi = self
            .builder
            .build_phi(result_ty, &format!("{label_prefix}.result"))
            .unwrap();
        phi.add_incoming(&[
            (&ok_agg.as_basic_value_enum(), ok_end_bb),
            (&err_agg.as_basic_value_enum(), err_end_bb),
        ]);
        Ok(phi.as_basic_value())
    }

    /// Wrap a freshly-constructed network fd in a `Result[T, E]` where
    /// `T` is a single-`i32`-field struct (`TcpListener` / `TcpStream` /
    /// `TlsStream` / `WebSocket`) — the construction-method counterpart to
    /// `wrap_tcp_io_result` (phase-8 line 64 audit: bind / accept / connect
    /// return `Result`, not an `fd: -1` sentinel). On `fd >= 0` returns
    /// `Ok` with the fd zero-extended into payload word 0 (the seeded
    /// single-`i32` struct's reconstruction truncates it back at the
    /// destructure); on `fd < 0` returns `Err(E)` — a 2-word enum
    /// `{tag, i32}` packed into Result payload words 0/1.
    ///
    /// **Cause decoding (phase-8 line 74).** The negative `fd` is a
    /// Kāra-stable error code from `net_construct_error_code` (runtime):
    /// `-2` → `AddrInUse`, `-3` → `ConnectionRefused`, `-4` →
    /// `PermissionDenied` (fieldless variants), and any other code →
    /// `err_variant_name` (the caller-supplied default — `Other` for the
    /// TCP surface, `Protocol` for TLS) carrying the code in its `i32`
    /// payload. The named-variant mapping is applied only when the target
    /// enum declares those variants (`TcpError` / `TlsError` do), so the
    /// helper degrades to "always the default variant" for any future
    /// caller whose enum lacks them. `T`'s identity comes from the call's
    /// typechecker-resolved Result type, so this helper need not know
    /// which single-fd struct it is wrapping.
    /// Decode the runtime's stable network-construction error code (`fd`,
    /// negative in this context) into the `(variant_tag, payload)` pair for
    /// an `Err(E)` aggregate (phase-8 line 74). Named causes map `-2 →
    /// AddrInUse`, `-3 → ConnectionRefused`, `-4 → PermissionDenied`
    /// (fieldless variants, when `err_enum_name` declares them) via a
    /// `select` chain; every other code maps to `default_variant_name`
    /// (`Other` for the TCP surface, `Protocol` for TLS) carrying the code
    /// sign-extended into the payload word (meaningful for the default
    /// variant, unread for the fieldless named ones). Shared by
    /// [`Self::build_fd_construct_result`] and
    /// `build_tls_listener_construct_result` so every network-construct Err
    /// arm classifies identically. Must be called with the builder
    /// positioned at the Err block (it emits the compares/selects inline).
    pub(super) fn classify_construct_err(
        &mut self,
        fd: inkwell::values::IntValue<'ctx>,
        err_enum_name: &str,
        default_variant_name: &str,
        label_prefix: &str,
    ) -> (
        inkwell::values::IntValue<'ctx>,
        inkwell::values::IntValue<'ctx>,
    ) {
        let i64_ty = self.context.i64_type();
        let err_layout = self
            .enum_layouts
            .get(err_enum_name)
            .unwrap_or_else(|| panic!("{err_enum_name} layout seeded"));
        let default_tag = *err_layout
            .tags
            .get(default_variant_name)
            .unwrap_or_else(|| panic!("{err_enum_name}.{default_variant_name} tag seeded"));
        // Copy the named-cause tags out so the `self.enum_layouts` borrow
        // ends before the builder calls below.
        let named = [
            (
                -2i64,
                err_layout.tags.get("AddrInUse").copied(),
                "addr_in_use",
            ),
            (
                -3i64,
                err_layout.tags.get("ConnectionRefused").copied(),
                "conn_refused",
            ),
            (
                -4i64,
                err_layout.tags.get("PermissionDenied").copied(),
                "perm_denied",
            ),
        ];

        // `fd` carries the runtime's negative error code here (i64 fd ABI);
        // build the comparison constants at the fd's own width.
        let fd_ty = fd.get_type();
        // Start from the default variant tag and wrap a `select` for each
        // named cause the target enum declares. Codes are mutually
        // exclusive, so chain order is immaterial.
        let mut variant_tag_val = i64_ty.const_int(default_tag, false);
        for (code, tag_opt, name) in named {
            let Some(tag) = tag_opt else { continue };
            let is_cause = self
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    fd,
                    fd_ty.const_int(code as u64, true),
                    &format!("{label_prefix}.err.is_{name}"),
                )
                .unwrap();
            variant_tag_val = self
                .builder
                .build_select(
                    is_cause,
                    i64_ty.const_int(tag, false),
                    variant_tag_val,
                    &format!("{label_prefix}.err.variant_tag.{name}"),
                )
                .unwrap()
                .into_int_value();
        }
        let payload = self
            .builder
            .build_int_s_extend(fd, i64_ty, &format!("{label_prefix}.err.code"))
            .unwrap();
        (variant_tag_val, payload)
    }

    pub(super) fn build_fd_construct_result(
        &mut self,
        fd: inkwell::values::IntValue<'ctx>,
        err_enum_name: &str,
        err_variant_name: &str,
        label_prefix: &str,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let ctx = self.context;
        let i64_ty = ctx.i64_type();

        let result_layout = self
            .enum_layouts
            .get("Result")
            .expect("Result layout seeded by seed_builtin_enum_layouts");
        let result_ty = result_layout.llvm_type;
        let ok_tag = *result_layout.tags.get("Ok").expect("Result.Ok tag seeded");
        let err_tag = *result_layout
            .tags
            .get("Err")
            .expect("Result.Err tag seeded");

        let fn_val = self
            .current_fn
            .ok_or_else(|| "network construct Result wrapping outside fn".to_string())?;
        let ok_bb = ctx.append_basic_block(fn_val, &format!("{label_prefix}.ok"));
        let err_bb = ctx.append_basic_block(fn_val, &format!("{label_prefix}.err"));
        let cont_bb = ctx.append_basic_block(fn_val, &format!("{label_prefix}.cont"));

        let is_success = self
            .builder
            .build_int_compare(
                IntPredicate::SGE,
                fd,
                fd.get_type().const_zero(),
                &format!("{label_prefix}.is_ok"),
            )
            .unwrap();
        self.builder
            .build_conditional_branch(is_success, ok_bb, err_bb)
            .unwrap();

        // ── Ok arm: Result.Ok(T) — tag at field 0; the i64 fd occupies
        //    payload word 0 (field 1) directly. The single-`i64` socket
        //    struct (TcpStream / TcpListener / WebSocket / Tls*) reads this
        //    word back as its `fd` field with no width change (i64 fd ABI —
        //    a Windows `SOCKET` is pointer-sized; the prior i32 path
        //    zero-extended here and truncated on reconstruction).
        self.builder.position_at_end(ok_bb);
        let fd_word = fd;
        let mut ok_agg = result_ty.get_undef();
        ok_agg = self
            .builder
            .build_insert_value(
                ok_agg,
                i64_ty.const_int(ok_tag, false),
                0,
                &format!("{label_prefix}.ok.tag"),
            )
            .unwrap()
            .into_struct_value();
        ok_agg = self
            .builder
            .build_insert_value(ok_agg, fd_word, 1, &format!("{label_prefix}.ok.fd"))
            .unwrap()
            .into_struct_value();
        let ok_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        // ── Err arm: Result.Err(E). The 2-word enum {variant_tag, payload}
        //    occupies Result payload words 0 and 1 (fields 1 and 2). The
        //    variant tag is decoded from the runtime's stable negative
        //    code (`fd`) via a select chain: each named code maps to its
        //    fieldless variant, every other code to the default variant
        //    (`err_variant_tag`). The payload word carries the code
        //    sign-extended to i64 — meaningful for the default `Other` /
        //    `Protocol(code)` variant and simply unread for the fieldless
        //    named variants (their `field_counts` is 0).
        self.builder.position_at_end(err_bb);
        let (variant_tag_val, payload) =
            self.classify_construct_err(fd, err_enum_name, err_variant_name, label_prefix);
        let mut err_agg = result_ty.get_undef();
        err_agg = self
            .builder
            .build_insert_value(
                err_agg,
                i64_ty.const_int(err_tag, false),
                0,
                &format!("{label_prefix}.err.tag"),
            )
            .unwrap()
            .into_struct_value();
        err_agg = self
            .builder
            .build_insert_value(
                err_agg,
                variant_tag_val,
                1,
                &format!("{label_prefix}.err.variant_tag"),
            )
            .unwrap()
            .into_struct_value();
        err_agg = self
            .builder
            .build_insert_value(err_agg, payload, 2, &format!("{label_prefix}.err.payload"))
            .unwrap()
            .into_struct_value();
        let err_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        // ── Continuation: phi between Ok and Err arms.
        self.builder.position_at_end(cont_bb);
        let phi = self
            .builder
            .build_phi(result_ty, &format!("{label_prefix}.result"))
            .unwrap();
        phi.add_incoming(&[
            (&ok_agg.as_basic_value_enum(), ok_end_bb),
            (&err_agg.as_basic_value_enum(), err_end_bb),
        ]);
        Ok(phi.as_basic_value())
    }

    /// Extract the single `i64 fd` field from a `TcpListener` /
    /// `TcpStream` struct receiver. Handles both struct-value (owned /
    /// move) and pointer (ref self) receiver shapes. (i64 fd ABI.)
    fn extract_fd_from_tcp_struct(
        &self,
        self_val: BasicValueEnum<'ctx>,
        name_hint: &str,
    ) -> inkwell::values::IntValue<'ctx> {
        if self_val.is_pointer_value() {
            let struct_ty = self
                .context
                .struct_type(&[self.context.i64_type().into()], false);
            let ptr_hint = format!("{name_hint}.ptr");
            let fd_ptr = self
                .builder
                .build_struct_gep(struct_ty, self_val.into_pointer_value(), 0, &ptr_hint)
                .expect("GEP fd field of Tcp struct via ref self pointer");
            self.builder
                .build_load(self.context.i64_type(), fd_ptr, name_hint)
                .expect("load fd from Tcp struct via ref self")
                .into_int_value()
        } else {
            self.builder
                .build_extract_value(self_val.into_struct_value(), 0, name_hint)
                .expect("extract fd from Tcp struct value")
                .into_int_value()
        }
    }

    // ── Phase 6 line 17 slice 9e.1 — WebSocket framing lowerings ────────
    //
    // `WebSocket` shares the same single-i32-field layout as
    // `TcpListener` / `TcpStream`, so the fd-extraction helper and
    // struct-build pattern transplant directly. The compose-at-leaf
    // shape from slice 8/9 also applies: park via
    // `karac_park_on_fd(self.fd, direction)` then call the
    // `karac_runtime_ws_*` FFI. The Result wrapping reuses
    // `wrap_tcp_io_result` because the runtime FFIs return the same
    // shape — `>= 0` for byte count, `-1` for error. v1 maps `-1` to
    // `TcpError.Other(-1)` (which `wrap_tcp_io_result` produces when
    // errno=-1 doesn't match EINTR=4); slice 9e.3's richer `WsError`
    // type with `Protocol` / `Closed` variants will require a
    // dedicated `wrap_ws_io_result` helper that distinguishes
    // EOF (0) from byte-count-zero (also 0 — they overlap in v1).

    /// Lower `WebSocket.accept(listener: TcpListener) -> WebSocket`
    /// — extract `listener.fd`, park on read-readiness, call the
    /// runtime FFI `karac_runtime_ws_accept` which performs the
    /// blocking accept(2) + HTTP-upgrade exchange, pack the
    /// returned conn fd into a `WebSocket { fd }` struct value.
    /// Mirror of `lower_tcp_listener_accept` but routes to the
    /// WS FFI instead of the raw TCP accept. Phase 6 line 17
    /// slice 9e.2.
    pub(super) fn lower_websocket_accept(
        &mut self,
        listener_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let listener_fd = self.extract_fd_from_tcp_struct(listener_val, "ws.accept.listener.fd");

        // Park on listener-readability (direction = 0 for read).
        let direction = self.context.i8_type().const_int(0, false);
        self.emit_state_machine_invocation_for_park_on_fd(listener_fd, direction);

        let accept_fn = self
            .module
            .get_function("karac_runtime_ws_accept")
            .expect("karac_runtime_ws_accept declared in Codegen::new");
        let conn_fd_call = self
            .builder
            .build_call(accept_fn, &[listener_fd.into()], "ws.accept.conn_fd")
            .expect("call karac_runtime_ws_accept");
        let conn_fd = conn_fd_call
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();

        // Pack into `Result[WebSocket, TcpError]` (phase-8 line 64 audit):
        // `Ok(WebSocket { fd })` on `fd >= 0`, else `Err(TcpError.Other(-1))`.
        self.build_fd_construct_result(conn_fd, "TcpError", "Other", "ws.accept")
    }

    /// Lower `WebSocket.from_fd(fd: i64) -> WebSocket` — pack the i64
    /// fd into a fresh `WebSocket { fd }` struct value. Mirror of the
    /// post-bind `insert_value` pack in `lower_tcp_listener_bind`. No
    /// syscall, no parking — pure value construction.
    pub(super) fn lower_websocket_from_fd(
        &mut self,
        fd_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let ws_ty = self
            .context
            .struct_type(&[self.context.i64_type().into()], false);
        // Kara's value model represents int-typed values as `i64` at the
        // LLVM SSA layer, and the WebSocket struct's `fd` field is now
        // `i64` (i64 fd ABI — a Windows `SOCKET` is pointer-sized), so the
        // fd inserts directly with no truncation (the prior i32-field
        // convention truncated here).
        let fd_int = fd_val.into_int_value();
        let undef = ws_ty.get_undef();
        let ws_val = self
            .builder
            .build_insert_value(undef, fd_int, 0, "ws.from_fd.val")
            .expect("insert fd into WebSocket struct value");
        Ok(ws_val.into_struct_value().into())
    }

    /// Lower `WebSocket.send_text(ref self, msg: Slice[u8]) ->
    /// Result[i64, TcpError]` — extract self.fd + msg.{ptr, len}, park
    /// on write-readiness, call `karac_runtime_ws_send_text`, wrap the
    /// returned `i64` in `Result[i64, TcpError]`. Mirror of
    /// `lower_tcp_stream_io` with `is_write=true` but routes to the
    /// WS FFI instead of `karac_runtime_tcp_write`.
    pub(super) fn lower_websocket_send_text(
        &mut self,
        self_val: BasicValueEnum<'ctx>,
        msg_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        self.lower_websocket_io(self_val, msg_val, "karac_runtime_ws_send_text", true)
    }

    /// Lower `WebSocket.recv_text(ref self, buf: mut Slice[u8]) ->
    /// Result[i64, TcpError]` — symmetric to `send_text` but parks on
    /// read-readiness and routes to `karac_runtime_ws_recv_text`.
    pub(super) fn lower_websocket_recv_text(
        &mut self,
        self_val: BasicValueEnum<'ctx>,
        buf_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        self.lower_websocket_io(self_val, buf_val, "karac_runtime_ws_recv_text", false)
    }

    /// Slice 9e.3 — binary frame send: same shape as `send_text`
    /// but routes to the binary FFI (opcode 0x2 in the runtime
    /// helper's frame header).
    pub(super) fn lower_websocket_send_binary(
        &mut self,
        self_val: BasicValueEnum<'ctx>,
        msg_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        self.lower_websocket_io(self_val, msg_val, "karac_runtime_ws_send_binary", true)
    }

    /// Slice 9e.3 — binary frame recv: same shape as `recv_text`
    /// but routes to the binary FFI.
    pub(super) fn lower_websocket_recv_binary(
        &mut self,
        self_val: BasicValueEnum<'ctx>,
        buf_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        self.lower_websocket_io(self_val, buf_val, "karac_runtime_ws_recv_binary", false)
    }

    /// Slice 9e.4 — client-side masked text send. Routes to the
    /// masked-send FFI which generates a random 4-byte mask key
    /// and emits a MASK=1 frame per RFC 6455 §5.1.
    pub(super) fn lower_websocket_send_text_masked(
        &mut self,
        self_val: BasicValueEnum<'ctx>,
        msg_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        self.lower_websocket_io(self_val, msg_val, "karac_runtime_ws_send_text_masked", true)
    }

    /// Slice 9e.4 — client-side masked binary send.
    pub(super) fn lower_websocket_send_binary_masked(
        &mut self,
        self_val: BasicValueEnum<'ctx>,
        msg_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        self.lower_websocket_io(
            self_val,
            msg_val,
            "karac_runtime_ws_send_binary_masked",
            true,
        )
    }

    /// Shared lowering for `WebSocket.{send,recv}_{text,binary}`
    /// — near-verbatim mirror of `lower_tcp_stream_io`. Extract
    /// `self.fd` plus `slice.{ptr, len}`, park on the right
    /// direction, call the supplied FFI, wrap the `i64` result
    /// via `wrap_tcp_io_result` (the WS FFIs follow the same
    /// `>= 0 / -1` convention as the TCP ones).
    fn lower_websocket_io(
        &mut self,
        self_val: BasicValueEnum<'ctx>,
        slice_val: BasicValueEnum<'ctx>,
        fn_name: &str,
        is_send: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fd = self.extract_fd_from_tcp_struct(self_val, "ws.io.self.fd");

        let slice_sv = slice_val.into_struct_value();
        let buf_ptr = self
            .builder
            .build_extract_value(slice_sv, 0, "ws.io.buf.ptr")
            .unwrap()
            .into_pointer_value();
        let buf_len = self
            .builder
            .build_extract_value(slice_sv, 1, "ws.io.buf.len")
            .unwrap()
            .into_int_value();

        let direction = self
            .context
            .i8_type()
            .const_int(if is_send { 1 } else { 0 }, false);
        self.emit_state_machine_invocation_for_park_on_fd(fd, direction);

        let io_fn = self
            .module
            .get_function(fn_name)
            .unwrap_or_else(|| panic!("{fn_name} declared in Codegen::new"));
        let label = if is_send { "ws.send.n" } else { "ws.recv.n" };
        let io_call = self
            .builder
            .build_call(io_fn, &[fd.into(), buf_ptr.into(), buf_len.into()], label)
            .unwrap_or_else(|_| panic!("call {fn_name}"));
        let n = io_call.try_as_basic_value().unwrap_basic().into_int_value();
        // `is_write` arg controls only the BB label prefix in
        // `wrap_tcp_io_result` ("tcp.write" vs "tcp.read"); the
        // labels show up in WS IR as "tcp.write" / "tcp.read", which
        // is mildly imprecise but harmless. A dedicated
        // `wrap_ws_io_result` with WS-specific labels could land
        // alongside the `WsError` introduction (slice 9e.4).
        self.wrap_tcp_io_result(n, is_send)
    }
}
