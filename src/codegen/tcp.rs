//! Codegen for stdlib `TcpListener` (`runtime/stdlib/tcp.kara`).
//!
//! Two `#[compiler_builtin]` methods:
//!
//! - `TcpListener.bind(addr: String) -> TcpListener` — calls
//!   `karac_runtime_tcp_bind(addr_ptr, addr_len) -> i32`, then wraps
//!   the returned fd in a fresh `TcpListener { fd }` struct value.
//!   The runtime prints `BOUND_PORT=<n>` to stdout when the requested
//!   address ends in `:0` (ephemeral-port convention, matching
//!   `Server.serve_static`).
//!
//! - `TcpListener.accept(ref self) -> i32` — Path A (per the Slice 6
//!   design call): parks via `karac_park_on_fd(self.fd, 0u8)` so the
//!   yield happens at the kara state-machine level, then calls
//!   `karac_runtime_tcp_accept(self.fd)` for the raw `accept(2)` to
//!   pick up the now-readable connection. The returned i32 is the new
//!   connection's raw fd (-1 on error). The parking step composes
//!   through the same `emit_state_machine_invocation_for_park_on_fd`
//!   helper that future stdlib yielding methods
//!   (`TcpStream.read` / `.write`, `WebSocket.recv` / `.send`, …)
//!   will reuse.
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

use inkwell::values::BasicValueEnum;
use inkwell::{AddressSpace, IntPredicate};

use super::declarations::KARAC_PARK_ON_FD;

impl<'ctx> super::Codegen<'ctx> {
    /// Emit the state-machine invocation pattern for `karac_park_on_fd`
    /// inline at the current builder position: allocate the state
    /// struct via the constructor, store `fd` (field 1) and `direction`
    /// (field 2), drive the poll loop with `sched_yield` on Pending,
    /// free the state struct on Ready. Mirrors slice 8d/8e's caller-
    /// side intercept body but specialised to the parking primitive's
    /// two-arg owned-param shape (no ref/slice handling needed).
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

        // Poll loop: invoke poll-fn, sched_yield on Pending, fall
        // through on Ready.
        let loop_bb = self
            .context
            .append_basic_block(cur_fn, "kara.park.poll_loop");
        let yield_bb = self
            .context
            .append_basic_block(cur_fn, "kara.park.poll_yield");
        let done_bb = self
            .context
            .append_basic_block(cur_fn, "kara.park.poll_done");
        self.builder
            .build_unconditional_branch(loop_bb)
            .expect("br to park poll loop");

        self.builder.position_at_end(loop_bb);
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
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
        let i8_ty = self.context.i8_type();
        let is_pending = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                poll_result,
                i8_ty.const_int(0, false),
                "kara.park.is_pending",
            )
            .expect("icmp eq i8 poll_result, 0");
        self.builder
            .build_conditional_branch(is_pending, yield_bb, done_bb)
            .expect("br on park poll discriminant");

        self.builder.position_at_end(yield_bb);
        self.builder
            .build_call(self.sched_yield_fn, &[], "kara.park.yield_result")
            .expect("call sched_yield from park yield block");
        self.builder
            .build_unconditional_branch(loop_bb)
            .expect("br back to park poll loop after yield");

        self.builder.position_at_end(done_bb);
        self.builder
            .build_call(self.free_fn, &[state_ptr.into()], "")
            .expect("free karac_park_on_fd state struct");
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

        // Pack into TcpListener { fd: i32 } struct value. Constructed
        // via insert_value on an undef so the result is an SSA struct
        // value (matching how other stdlib types — `Pool { handle_id }`
        // — return from compiler-builtin lowerings).
        let listener_ty = self
            .context
            .struct_type(&[self.context.i32_type().into()], false);
        let undef = listener_ty.get_undef();
        let listener_val = self
            .builder
            .build_insert_value(undef, fd, 0, "tcp.listener.val")
            .expect("insert fd into TcpListener struct value");
        Ok(listener_val.into_struct_value().into())
    }

    /// Lower `TcpListener.accept(ref self) -> i32` to: park on the
    /// listener's fd (via `karac_park_on_fd(self.fd, 0u8)`), then
    /// call `karac_runtime_tcp_accept(self.fd)` for the raw accept(2).
    /// Returns the new connection fd (-1 on error).
    pub(super) fn lower_tcp_listener_accept(
        &mut self,
        self_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Extract self.fd. `ref self` lowers as a pointer in the
        // owned-receiver case (the caller passes the struct value
        // through); for the simplest v1 surface we accept either the
        // struct value directly or a pointer to it.
        let fd = if self_val.is_pointer_value() {
            let listener_ty = self
                .context
                .struct_type(&[self.context.i32_type().into()], false);
            let fd_ptr = self
                .builder
                .build_struct_gep(
                    listener_ty,
                    self_val.into_pointer_value(),
                    0,
                    "tcp.accept.self.fd.ptr",
                )
                .expect("GEP fd field of TcpListener via ref self pointer");
            self.builder
                .build_load(self.context.i32_type(), fd_ptr, "tcp.accept.self.fd")
                .expect("load fd from TcpListener via ref self")
                .into_int_value()
        } else {
            self.builder
                .build_extract_value(self_val.into_struct_value(), 0, "tcp.accept.self.fd")
                .expect("extract fd from TcpListener struct value")
                .into_int_value()
        };

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
        Ok(accept_call.try_as_basic_value().unwrap_basic())
    }
}
