//! Codegen for stdlib `TlsListener` / `TlsStream` (`runtime/stdlib/tls.kara`).
//!
//! Phase 6 line 236 slice 2 — kara-side surface for the rustls FFI
//! shipped in slice 1 (`runtime/src/tls.rs`). The lowerings parallel
//! `src/codegen/tcp.rs` exactly because the TLS stdlib types mirror
//! the TCP ones at the kara surface:
//!
//! | Kara method | Lowering | Runtime FFI |
//! |---|---|---|
//! | `TlsListener.bind_tls(addr, cert, key)` | `lower_tls_listener_bind_tls` | `karac_runtime_tls_config_new` + `_tls_listener_bind` |
//! | `TlsListener.accept(ref self)` | `lower_tls_listener_accept` | park + `karac_runtime_tls_accept` |
//! | `TlsStream.read(ref self, mut Slice[u8])` | `lower_tls_stream_read` | park + `karac_runtime_tls_read` |
//! | `TlsStream.write(ref self, Slice[u8])` | `lower_tls_stream_write` | park + `karac_runtime_tls_write` |
//! | `TlsStream.write_all(ref self, Slice[u8])` | `lower_tls_stream_write_all` | loop `_tls_write` until done |
//! | Drop for TlsListener | `emit_tls_listener_drop_body` | `_tls_config_free` + `_tls_close` |
//! | Drop for TlsStream | `emit_tls_stream_drop_body` | `_tls_close` |
//!
//! **TlsListener struct shape (LLVM):** `{ i32 fd, ptr config }`. The
//! `config` field carries the `*mut TlsConfig` returned by
//! `karac_runtime_tls_config_new` at bind-time; codegen extracts both
//! fields at accept and feeds them into `karac_runtime_tls_accept(fd,
//! config)`. `TlsStream` is `{ i32 fd }` — identical to `TcpStream`,
//! since the TLS session state lives in the runtime-side `SESSIONS`
//! registry keyed by fd (see `runtime/src/tls.rs` `## Session storage`).
//!
//! **Result wrapping reuses `wrap_tcp_io_result`** because the TLS FFI
//! returns the same negative-errno-on-failure convention as the TCP
//! FFI. `-1` (the TLS-specific non-syscall error sentinel) maps to
//! `TcpError.Other(1)` since `wrap_tcp_io_result` reads `errno = -n`
//! (so n=-1 → errno=1 → not EINTR → `Other(1)`). For v1 this is
//! "any error other than EINTR is unrecoverable" — sufficient for
//! Demo 1 slice 2; a dedicated `TlsError` enum with distinct
//! protocol-vs-syscall variants is a slice-2 follow-on if a workload
//! needs the structured distinction.

use inkwell::values::{BasicValue, BasicValueEnum};
use inkwell::{AddressSpace, IntPredicate};

impl<'ctx> super::Codegen<'ctx> {
    /// Lower `TlsListener.bind_tls(addr: String, cert_pem: String, key_pem: String)
    /// -> TlsListener` to:
    ///   1. Extract `{ptr, len}` from each of the three String args.
    ///   2. Call `karac_runtime_tls_config_new(cert_ptr, cert_len, key_ptr,
    ///      key_len) -> *mut TlsConfig`. On failure (null return) the
    ///      `TlsListener.config` field carries null; subsequent accepts
    ///      will fail with `tls_accept` returning -1 because the FFI
    ///      rejects null configs.
    ///   3. Call `karac_runtime_tls_listener_bind(addr_ptr, addr_len,
    ///      config) -> i32`. `:0` ephemeral-port + BOUND_PORT-print
    ///      convention lives runtime-side (delegates to
    ///      `karac_runtime_tcp_bind`).
    ///   4. Pack into a `TlsListener { fd: i32, config: ptr }` struct
    ///      value via two insert_value ops on an undef.
    pub(super) fn lower_tls_listener_bind_tls(
        &mut self,
        addr_val: BasicValueEnum<'ctx>,
        cert_val: BasicValueEnum<'ctx>,
        key_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let (cert_ptr, cert_len) = self.extract_string_ptr_len(cert_val, "tls.bind.cert");
        let (key_ptr, key_len) = self.extract_string_ptr_len(key_val, "tls.bind.key");
        let (addr_ptr, addr_len) = self.extract_string_ptr_len(addr_val, "tls.bind.addr");

        let config_new_fn = self
            .module
            .get_function("karac_runtime_tls_config_new")
            .expect("karac_runtime_tls_config_new declared in Codegen::new");
        let config_call = self
            .builder
            .build_call(
                config_new_fn,
                &[
                    cert_ptr.into(),
                    cert_len.into(),
                    key_ptr.into(),
                    key_len.into(),
                ],
                "tls.bind.config",
            )
            .expect("call karac_runtime_tls_config_new");
        let config_ptr = config_call
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        let listener_bind_fn = self
            .module
            .get_function("karac_runtime_tls_listener_bind")
            .expect("karac_runtime_tls_listener_bind declared in Codegen::new");
        let bind_call = self
            .builder
            .build_call(
                listener_bind_fn,
                &[addr_ptr.into(), addr_len.into(), config_ptr.into()],
                "tls.bind.fd",
            )
            .expect("call karac_runtime_tls_listener_bind");
        let fd = bind_call
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();

        let listener_ty = self.tls_listener_llvm_type();
        let undef = listener_ty.get_undef();
        let after_fd = self
            .builder
            .build_insert_value(undef, fd, 0, "tls.listener.with_fd")
            .expect("insert fd into TlsListener struct value");
        let listener_val = self
            .builder
            .build_insert_value(after_fd, config_ptr, 1, "tls.listener.val")
            .expect("insert config_ptr into TlsListener struct value");
        Ok(listener_val.into_struct_value().into())
    }

    /// Phase-8 line 22 — lower `TlsStream.connect(addr: String,
    /// server_name: String, roots_pem: String) -> TlsStream`. Client-
    /// side counterpart of `TlsListener.bind_tls` + `.accept`:
    ///   1. Extract `(ptr, len)` from each of the three String args.
    ///   2. Call `karac_runtime_tls_client_connect(addr_ptr, addr_len,
    ///      server_name_ptr, server_name_len, roots_pem_ptr,
    ///      roots_pem_len) -> i32` — TCP connect + sync rustls
    ///      handshake + register the `ClientConnection` in the shared
    ///      per-fd session map. Returns the post-handshake fd or -1.
    ///   3. Pack into `TlsStream { fd: i32 }` (same single-i32 layout
    ///      `accept` produces; user code can't tell which side of the
    ///      connection the stream came from at the type level — the
    ///      runtime treats both directions uniformly).
    ///
    /// On any failure (addr parse, server-name parse, PEM parse, TCP
    /// connect, handshake) the FFI returns -1; the resulting
    /// `TlsStream { fd: -1 }` surfaces as a write/read error on first
    /// use (`Err(TcpError)` via `wrap_tcp_io_result`).
    pub(super) fn lower_tls_stream_connect(
        &mut self,
        addr_val: BasicValueEnum<'ctx>,
        server_name_val: BasicValueEnum<'ctx>,
        roots_pem_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let (addr_ptr, addr_len) = self.extract_string_ptr_len(addr_val, "tls.connect.addr");
        let (server_name_ptr, server_name_len) =
            self.extract_string_ptr_len(server_name_val, "tls.connect.name");
        let (roots_ptr, roots_len) =
            self.extract_string_ptr_len(roots_pem_val, "tls.connect.roots");

        let connect_fn = self
            .module
            .get_function("karac_runtime_tls_client_connect")
            .expect("karac_runtime_tls_client_connect declared in Codegen::new");
        let connect_call = self
            .builder
            .build_call(
                connect_fn,
                &[
                    addr_ptr.into(),
                    addr_len.into(),
                    server_name_ptr.into(),
                    server_name_len.into(),
                    roots_ptr.into(),
                    roots_len.into(),
                ],
                "tls.connect.fd",
            )
            .expect("call karac_runtime_tls_client_connect");
        let fd = connect_call
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();

        // Same `TlsStream { fd: i32 }` shape `accept` produces.
        let stream_ty = self
            .context
            .struct_type(&[self.context.i32_type().into()], false);
        let undef = stream_ty.get_undef();
        let stream_val = self
            .builder
            .build_insert_value(undef, fd, 0, "tls.connect.stream.val")
            .expect("insert fd into TlsStream struct value");
        Ok(stream_val.into_struct_value().into())
    }

    /// Phase 6 line 236 slice 3 — `WebSocket.accept_tls(listener:
    /// TlsListener) -> WebSocket`. Composes a TLS-wrapped accept +
    /// WS HTTP upgrade in one shot through
    /// `karac_runtime_ws_accept_tls(listener_fd, config)`. The
    /// kara-level state machine yields on listener-readability via
    /// `karac_park_on_fd(listener.fd, 0u8)` before the handshake.
    ///
    /// Identical shape to `lower_websocket_accept` (plain TCP path)
    /// except (a) the listener is `TlsListener` so we extract both
    /// fd and config_ptr, (b) the runtime FFI is the TLS-aware
    /// variant which additionally registers the connection in the
    /// per-fd TLS session registry so subsequent `recv_text` /
    /// `send_text` calls auto-route through TLS.
    pub(super) fn lower_websocket_accept_tls(
        &mut self,
        listener_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let (listener_fd, config_ptr) =
            self.extract_fd_and_config_from_tls_listener(listener_val, "ws.accept_tls.listener");

        let direction = self.context.i8_type().const_int(0, false);
        self.emit_state_machine_invocation_for_park_on_fd(listener_fd, direction);

        let accept_fn = self
            .module
            .get_function("karac_runtime_ws_accept_tls")
            .expect("karac_runtime_ws_accept_tls declared in Codegen::new");
        let conn_fd_call = self
            .builder
            .build_call(
                accept_fn,
                &[listener_fd.into(), config_ptr.into()],
                "ws.accept_tls.conn_fd",
            )
            .expect("call karac_runtime_ws_accept_tls");
        let conn_fd = conn_fd_call
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();

        let ws_ty = self
            .context
            .struct_type(&[self.context.i32_type().into()], false);
        let undef = ws_ty.get_undef();
        let ws_val = self
            .builder
            .build_insert_value(undef, conn_fd, 0, "ws.accept_tls.val")
            .expect("insert conn_fd into WebSocket struct value");
        Ok(ws_val.into_struct_value().into())
    }

    /// Lower `TlsListener.accept(ref self) -> TlsStream`: park on
    /// `self.fd` for read-readiness (direction = 0), then call
    /// `karac_runtime_tls_accept(self.fd, self.config)` for the raw
    /// accept(2) + synchronous TLS handshake. Returns a
    /// `TlsStream { fd }` wrapping the connection fd; on handshake
    /// failure the FFI returns -1 surfacing as `TlsStream { fd: -1 }`.
    pub(super) fn lower_tls_listener_accept(
        &mut self,
        self_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let (fd, config_ptr) = self.extract_fd_and_config_from_tls_listener(self_val, "tls.accept");

        let direction = self.context.i8_type().const_int(0, false);
        self.emit_state_machine_invocation_for_park_on_fd(fd, direction);

        let accept_fn = self
            .module
            .get_function("karac_runtime_tls_accept")
            .expect("karac_runtime_tls_accept declared in Codegen::new");
        let accept_call = self
            .builder
            .build_call(
                accept_fn,
                &[fd.into(), config_ptr.into()],
                "tls.accept.conn_fd",
            )
            .expect("call karac_runtime_tls_accept");
        let conn_fd = accept_call
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();

        // Pack into TlsStream { fd: i32 } — single-i32 layout matches
        // TcpStream so the `types_lowering.rs` baked-stdlib arm can
        // group them under the same `{ i32 }` shape.
        let stream_ty = self
            .context
            .struct_type(&[self.context.i32_type().into()], false);
        let undef = stream_ty.get_undef();
        let stream_val = self
            .builder
            .build_insert_value(undef, conn_fd, 0, "tls.stream.val")
            .expect("insert fd into TlsStream struct value");
        Ok(stream_val.into_struct_value().into())
    }

    /// Lower `TlsStream.read(ref self, buf: mut Slice[u8])` —
    /// near-verbatim mirror of `lower_tcp_stream_io(read)` since
    /// `TlsStream` is `{ i32 fd }` like `TcpStream`. The runtime FFI
    /// pumps rustls's inbound packet processor until plaintext is
    /// available; the `Result[i64, TcpError]` wrapping is the same.
    pub(super) fn lower_tls_stream_read(
        &mut self,
        self_val: BasicValueEnum<'ctx>,
        buf_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        self.lower_tls_stream_io(self_val, buf_val, /*is_write=*/ false)
    }

    /// Lower `TlsStream.write(ref self, buf: Slice[u8])` — mirror of
    /// `lower_tcp_stream_io(write)`. rustls's writer never short-writes
    /// so the byte count returned always equals `buf.len()` on
    /// success, but the Result-wrapping shape matches TCP for
    /// uniformity.
    pub(super) fn lower_tls_stream_write(
        &mut self,
        self_val: BasicValueEnum<'ctx>,
        buf_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        self.lower_tls_stream_io(self_val, buf_val, /*is_write=*/ true)
    }

    /// Shared lowering for `TlsStream.read` / `.write`: same shape as
    /// `lower_tcp_stream_io` but routes to `karac_runtime_tls_read /
    /// _tls_write`. Uses the shared `wrap_tcp_io_result` helper for
    /// the `Result[i64, TcpError]` wrapping (TCP and TLS FFIs share
    /// the negative-errno return convention).
    fn lower_tls_stream_io(
        &mut self,
        self_val: BasicValueEnum<'ctx>,
        buf_val: BasicValueEnum<'ctx>,
        is_write: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fd = self.extract_fd_from_tls_stream(self_val, "tls.io.self.fd");

        let buf_sv = buf_val.into_struct_value();
        let buf_ptr = self
            .builder
            .build_extract_value(buf_sv, 0, "tls.io.buf.ptr")
            .unwrap()
            .into_pointer_value();
        let buf_len = self
            .builder
            .build_extract_value(buf_sv, 1, "tls.io.buf.len")
            .unwrap()
            .into_int_value();

        let direction = self
            .context
            .i8_type()
            .const_int(if is_write { 1 } else { 0 }, false);
        self.emit_state_machine_invocation_for_park_on_fd(fd, direction);

        let fn_name = if is_write {
            "karac_runtime_tls_write"
        } else {
            "karac_runtime_tls_read"
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
                    "tls.write.n"
                } else {
                    "tls.read.n"
                },
            )
            .unwrap_or_else(|_| panic!("call {fn_name}"));
        let n = io_call.try_as_basic_value().unwrap_basic().into_int_value();
        self.wrap_tcp_io_result(n, is_write)
    }

    /// Lower `TlsStream.write_all(ref self, buf: Slice[u8])` — same
    /// retry-on-EINTR loop as `lower_tcp_stream_write_all`, just
    /// routes through `karac_runtime_tls_write` instead. Code shape
    /// is byte-for-byte identical aside from the FFI name; kept
    /// separate rather than parameterised to keep diagnostics
    /// (`tls.wa.*` block names) distinct in IR dumps.
    pub(super) fn lower_tls_stream_write_all(
        &mut self,
        self_val: BasicValueEnum<'ctx>,
        buf_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let ctx = self.context;
        let i64_ty = ctx.i64_type();
        let i8_ty = ctx.i8_type();
        let zero_i64 = i64_ty.const_zero();

        let fd = self.extract_fd_from_tls_stream(self_val, "tls.wa.self.fd");

        let buf_sv = buf_val.into_struct_value();
        let buf_ptr = self
            .builder
            .build_extract_value(buf_sv, 0, "tls.wa.buf.ptr")
            .unwrap()
            .into_pointer_value();
        let buf_len = self
            .builder
            .build_extract_value(buf_sv, 1, "tls.wa.buf.len")
            .unwrap()
            .into_int_value();

        let fn_val = self
            .current_fn
            .ok_or_else(|| "TlsStream.write_all lowered outside fn".to_string())?;

        let written_slot = self.create_entry_alloca(fn_val, "tls.wa.written", i64_ty.into());
        self.builder.build_store(written_slot, zero_i64).unwrap();

        let loop_head = ctx.append_basic_block(fn_val, "tls.wa.loop.head");
        let loop_body = ctx.append_basic_block(fn_val, "tls.wa.loop.body");
        let advance = ctx.append_basic_block(fn_val, "tls.wa.advance");
        let err_check = ctx.append_basic_block(fn_val, "tls.wa.err.check");
        let err_exit = ctx.append_basic_block(fn_val, "tls.wa.err.exit");
        let ok_exit = ctx.append_basic_block(fn_val, "tls.wa.ok.exit");
        let cont = ctx.append_basic_block(fn_val, "tls.wa.cont");

        self.builder.build_unconditional_branch(loop_head).unwrap();

        // ── loop_head: if written >= buf.len, exit Ok; else body.
        self.builder.position_at_end(loop_head);
        let written = self
            .builder
            .build_load(i64_ty, written_slot, "tls.wa.written.load")
            .unwrap()
            .into_int_value();
        let is_done = self
            .builder
            .build_int_compare(IntPredicate::SGE, written, buf_len, "tls.wa.is_done")
            .unwrap();
        self.builder
            .build_conditional_branch(is_done, ok_exit, loop_body)
            .unwrap();

        // ── loop_body: chunk_ptr = buf.ptr + written, remaining = buf.len - written,
        //    park on write-readiness, call FFI, branch on success.
        self.builder.position_at_end(loop_body);
        let remaining = self
            .builder
            .build_int_sub(buf_len, written, "tls.wa.remaining")
            .unwrap();
        let chunk_ptr = unsafe {
            self.builder
                .build_in_bounds_gep(i8_ty, buf_ptr, &[written], "tls.wa.chunk.ptr")
                .unwrap()
        };
        let dir_write = i8_ty.const_int(1, false);
        self.emit_state_machine_invocation_for_park_on_fd(fd, dir_write);

        let write_fn = self
            .module
            .get_function("karac_runtime_tls_write")
            .expect("karac_runtime_tls_write declared in Codegen::new");
        let write_call = self
            .builder
            .build_call(
                write_fn,
                &[fd.into(), chunk_ptr.into(), remaining.into()],
                "tls.wa.write.n",
            )
            .expect("call karac_runtime_tls_write");
        let n = write_call
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let is_ok = self
            .builder
            .build_int_compare(IntPredicate::SGE, n, zero_i64, "tls.wa.is_ok")
            .unwrap();
        self.builder
            .build_conditional_branch(is_ok, advance, err_check)
            .unwrap();

        // ── advance: written += n, br loop_head.
        self.builder.position_at_end(advance);
        let new_written = self
            .builder
            .build_int_add(written, n, "tls.wa.new_written")
            .unwrap();
        self.builder.build_store(written_slot, new_written).unwrap();
        self.builder.build_unconditional_branch(loop_head).unwrap();

        // ── err_check: errno = -n; EINTR retries (back to loop_head, no advance),
        //    everything else exits with Err.
        self.builder.position_at_end(err_check);
        let errno = self
            .builder
            .build_int_sub(zero_i64, n, "tls.wa.errno")
            .unwrap();
        let eintr = i64_ty.const_int(4, false);
        let is_eintr = self
            .builder
            .build_int_compare(IntPredicate::EQ, errno, eintr, "tls.wa.is_eintr")
            .unwrap();
        self.builder
            .build_conditional_branch(is_eintr, loop_head, err_exit)
            .unwrap();
        let err_check_end_bb = self.builder.get_insert_block().unwrap();

        // ── err_exit: build Result.Err(TcpError.Other(errno)), br cont.
        self.builder.position_at_end(err_exit);
        let errno_phi = self.builder.build_phi(i64_ty, "tls.wa.errno.phi").unwrap();
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
                "tls.wa.err.tag",
            )
            .unwrap()
            .into_struct_value();
        err_agg = self
            .builder
            .build_insert_value(
                err_agg,
                i64_ty.const_int(other_tag, false),
                1,
                "tls.wa.err.tcp_err.w0",
            )
            .unwrap()
            .into_struct_value();
        let errno_phi_val = errno_phi.as_basic_value().into_int_value();
        err_agg = self
            .builder
            .build_insert_value(err_agg, errno_phi_val, 2, "tls.wa.err.tcp_err.w1")
            .unwrap()
            .into_struct_value();
        let err_exit_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(cont).unwrap();

        // ── ok_exit: build Result.Ok(written_final), br cont.
        self.builder.position_at_end(ok_exit);
        let written_final = self
            .builder
            .build_load(i64_ty, written_slot, "tls.wa.written.final")
            .unwrap()
            .into_int_value();
        let mut ok_agg = result_ty.get_undef();
        ok_agg = self
            .builder
            .build_insert_value(ok_agg, i64_ty.const_int(ok_tag, false), 0, "tls.wa.ok.tag")
            .unwrap()
            .into_struct_value();
        ok_agg = self
            .builder
            .build_insert_value(ok_agg, written_final, 1, "tls.wa.ok.n")
            .unwrap()
            .into_struct_value();
        let ok_exit_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(cont).unwrap();

        // ── cont: phi between err_exit and ok_exit, return phi.
        self.builder.position_at_end(cont);
        let phi = self.builder.build_phi(result_ty, "tls.wa.result").unwrap();
        phi.add_incoming(&[
            (&err_agg.as_basic_value_enum(), err_exit_end_bb),
            (&ok_agg.as_basic_value_enum(), ok_exit_end_bb),
        ]);
        Ok(phi.as_basic_value())
    }

    /// LLVM struct type for `TlsListener` — `{ i32 fd, ptr config }`.
    /// Built inline rather than read from `self.struct_types` because
    /// stdlib structs aren't registered there (same convention as
    /// `lower_tcp_listener_bind` for the `{ i32 }` shape). Used by
    /// `lower_tls_listener_bind_tls`, `extract_fd_and_config_from_tls_listener`,
    /// and `emit_tls_listener_drop_body`.
    pub(super) fn tls_listener_llvm_type(&self) -> inkwell::types::StructType<'ctx> {
        self.context.struct_type(
            &[
                self.context.i32_type().into(),
                self.context.ptr_type(AddressSpace::default()).into(),
            ],
            false,
        )
    }

    /// Extract the `(fd, config_ptr)` pair from a `TlsListener` struct
    /// receiver. Handles both struct-value (owned/move) and pointer
    /// (ref self) receiver shapes — same dispatch shape as
    /// `extract_fd_from_tcp_struct` in `tcp.rs`.
    fn extract_fd_and_config_from_tls_listener(
        &self,
        self_val: BasicValueEnum<'ctx>,
        name_hint: &str,
    ) -> (
        inkwell::values::IntValue<'ctx>,
        inkwell::values::PointerValue<'ctx>,
    ) {
        let i32_ty = self.context.i32_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        if self_val.is_pointer_value() {
            let struct_ty = self.tls_listener_llvm_type();
            let self_ptr = self_val.into_pointer_value();
            let fd_field_ptr = self
                .builder
                .build_struct_gep(struct_ty, self_ptr, 0, &format!("{name_hint}.fd_ptr"))
                .expect("GEP fd field of TlsListener via ref self");
            let fd = self
                .builder
                .build_load(i32_ty, fd_field_ptr, &format!("{name_hint}.fd"))
                .expect("load fd from TlsListener via ref self")
                .into_int_value();
            let config_field_ptr = self
                .builder
                .build_struct_gep(struct_ty, self_ptr, 1, &format!("{name_hint}.config_ptr"))
                .expect("GEP config field of TlsListener via ref self");
            let config = self
                .builder
                .build_load(ptr_ty, config_field_ptr, &format!("{name_hint}.config"))
                .expect("load config from TlsListener via ref self")
                .into_pointer_value();
            (fd, config)
        } else {
            let sv = self_val.into_struct_value();
            let fd = self
                .builder
                .build_extract_value(sv, 0, &format!("{name_hint}.fd"))
                .expect("extract fd from TlsListener struct value")
                .into_int_value();
            let config = self
                .builder
                .build_extract_value(sv, 1, &format!("{name_hint}.config"))
                .expect("extract config from TlsListener struct value")
                .into_pointer_value();
            (fd, config)
        }
    }

    /// Extract the single `i32 fd` field from a `TlsStream` struct
    /// receiver — identical to `extract_fd_from_tcp_struct` (the
    /// layouts are byte-for-byte the same). Kept separate so debug
    /// labels stay TLS-specific in IR dumps.
    fn extract_fd_from_tls_stream(
        &self,
        self_val: BasicValueEnum<'ctx>,
        name_hint: &str,
    ) -> inkwell::values::IntValue<'ctx> {
        if self_val.is_pointer_value() {
            let struct_ty = self
                .context
                .struct_type(&[self.context.i32_type().into()], false);
            let ptr_hint = format!("{name_hint}.ptr");
            let fd_ptr = self
                .builder
                .build_struct_gep(struct_ty, self_val.into_pointer_value(), 0, &ptr_hint)
                .expect("GEP fd field of TlsStream via ref self pointer");
            self.builder
                .build_load(self.context.i32_type(), fd_ptr, name_hint)
                .expect("load fd from TlsStream via ref self")
                .into_int_value()
        } else {
            self.builder
                .build_extract_value(self_val.into_struct_value(), 0, name_hint)
                .expect("extract fd from TlsStream struct value")
                .into_int_value()
        }
    }

    /// Helper: extract `{ptr, len}` from a Kāra `String` struct value
    /// (matches `{ptr, i64 len, i64 cap}` Vec-style layout). Used by
    /// `lower_tls_listener_bind_tls` to unpack the three String args.
    fn extract_string_ptr_len(
        &mut self,
        s_val: BasicValueEnum<'ctx>,
        name_hint: &str,
    ) -> (
        inkwell::values::PointerValue<'ctx>,
        inkwell::values::IntValue<'ctx>,
    ) {
        let sv = s_val.into_struct_value();
        let ptr = self
            .builder
            .build_extract_value(sv, 0, &format!("{name_hint}.ptr"))
            .unwrap()
            .into_pointer_value();
        let len = self
            .builder
            .build_extract_value(sv, 1, &format!("{name_hint}.len"))
            .unwrap()
            .into_int_value();
        (ptr, len)
    }
}
