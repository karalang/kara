//! Phase 6 line 236 slice 2 — stdlib `TlsListener` / `TlsStream`
//! codegen IR-grep tests.
//!
//! Mirrors `tests/ws_framing.rs`'s approach: codegen-side wire-up tests
//! (kara source → IR contains the right FFI calls / struct shapes) for
//! the methods slice 2 lowers. The wire-format correctness of
//! `karac_runtime_tls_*` is pinned by the runtime crate's unit tests
//! in `runtime/src/tls.rs`'s `tests` module; this file pins the
//! codegen routing.

#![cfg(feature = "llvm")]

mod tls_codegen_tests {
    use karac::codegen::compile_to_ir;

    fn ir_for(src: &str) -> String {
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        assert!(resolved.errors.is_empty(), "resolve: {:?}", resolved.errors);
        let typed = karac::typecheck(&parsed.program, &resolved);
        assert!(typed.errors.is_empty(), "typecheck: {:?}", typed.errors);
        karac::lower(&mut parsed.program, &typed);
        compile_to_ir(&parsed.program, None, None).expect("codegen failed")
    }

    /// Locate a function body in IR by name (shared with other test
    /// files in the same pattern). Returns the body text including the
    /// brace pairs, or `None` if the symbol isn't found.
    fn function_body(ir: &str, name: &str) -> Option<String> {
        let needle = format!("@{name}(");
        let mut found_define = false;
        let mut depth = 0i32;
        let mut body = String::new();
        for line in ir.lines() {
            if !found_define {
                if line.starts_with("define ") && line.contains(&needle) {
                    found_define = true;
                    depth = line.matches('{').count() as i32 - line.matches('}').count() as i32;
                    continue;
                }
            } else {
                body.push_str(line);
                body.push('\n');
                depth += line.matches('{').count() as i32;
                depth -= line.matches('}').count() as i32;
                if depth <= 0 {
                    return Some(body);
                }
            }
        }
        None
    }

    #[test]
    fn test_ir_tls_runtime_ffis_declared() {
        // Smoke check that the six TLS FFI symbols are declared in the
        // IR module — slice 2's lowerings dispatch by name, so missing
        // declarations would surface as `get_function(...).expect(...)`
        // panics inside the lowering. A trivial program that doesn't
        // call the FFIs still includes the declarations because they
        // sit in `Codegen::new`'s unconditional declaration pass.
        let ir = ir_for("fn main() {}");
        for name in [
            "karac_runtime_tls_config_new",
            "karac_runtime_tls_config_free",
            "karac_runtime_tls_listener_bind",
            "karac_runtime_tls_accept",
            "karac_runtime_tls_read",
            "karac_runtime_tls_write",
            "karac_runtime_tls_close",
            // Phase-8 line 22 — client connect FFI joins the same
            // unconditional-declaration set.
            "karac_runtime_tls_client_connect",
        ] {
            assert!(
                ir.contains("declare ") && ir.contains(name),
                "expected declaration of `{name}` in IR"
            );
        }
    }

    /// Phase-8 line 22 — `TlsStream.connect(addr, server_name,
    /// roots_pem)` lowers through `lower_tls_stream_connect` to a
    /// single `karac_runtime_tls_client_connect(addr_ptr, addr_len,
    /// name_ptr, name_len, roots_ptr, roots_len) -> i32` call, then
    /// packs the returned fd into a `TlsStream { i32 }` struct value.
    /// Pins the dispatch arm + extern wiring.
    #[test]
    fn test_ir_tls_stream_connect_dispatches_to_runtime_ffi() {
        let ir = ir_for(
            r#"
fn main() {
    let addr: String = "127.0.0.1:8443";
    let name: String = "localhost";
    let roots: String = "fake-pem";
    let stream: TlsStream = TlsStream.connect(addr, name, roots);
}
"#,
        );
        let main_body = function_body(&ir, "main").expect("main body");
        assert!(
            main_body.contains("call i32 @karac_runtime_tls_client_connect("),
            "main should call _tls_client_connect; body was:\n{}",
            main_body
        );
    }

    /// Phase-8 line 24 — `TlsStream.read` / `.write` wrap their FFI
    /// return through `wrap_tls_io_result`, producing `Result[i64,
    /// TlsError]`. The new codegen emits a distinctive `is_protocol`
    /// comparison + `tls_err.w0` label that pin the new path versus
    /// the previous `wrap_tcp_io_result` path.
    #[test]
    fn test_ir_tls_stream_read_wraps_with_tls_io_result() {
        let ir = ir_for(
            r#"
fn handle(s: ref TlsStream, buf: mut Slice[u8]) -> Result[i64, TlsError]
    with sends(Network) receives(Network)
{
    s.read(buf)
}
"#,
        );
        let body = function_body(&ir, "handle").expect("handle body");
        assert!(
            body.contains("tls.read.is_protocol"),
            "expected Protocol-vs-other classification select; body was:\n{}",
            body
        );
        assert!(
            body.contains("tls.read.err.tls_err.w0"),
            "expected TlsError tag word in Err arm; body was:\n{}",
            body
        );
    }

    /// `TlsStream.write_all`'s err_exit block now classifies
    /// `n == -1` as `Protocol(-1)` rather than `Other(1)`. Pins the
    /// `tls.wa.err.is_protocol` select in the IR.
    #[test]
    fn test_ir_tls_stream_write_all_classifies_protocol_in_err_exit() {
        let ir = ir_for(
            r#"
fn handle(s: ref TlsStream, buf: Slice[u8]) -> Result[i64, TlsError]
    with sends(Network) receives(Network)
{
    s.write_all(buf)
}
"#,
        );
        let body = function_body(&ir, "handle").expect("handle body");
        assert!(
            body.contains("tls.wa.err.is_protocol"),
            "write_all err_exit should classify Protocol vs Other; body was:\n{}",
            body
        );
    }

    #[test]
    fn test_ir_tls_bind_tls_dispatches_to_runtime_ffis() {
        // `TlsListener.bind_tls(addr, cert, key)` should lower to
        // `karac_runtime_tls_config_new(...)` followed by
        // `karac_runtime_tls_listener_bind(addr_ptr, addr_len, config)`.
        let ir = ir_for(
            r#"
fn main() {
    let addr: String = "127.0.0.1:0";
    let cert: String = "fake-cert";
    let key: String = "fake-key";
    let listener: TlsListener = TlsListener.bind_tls(addr, cert, key);
}
"#,
        );
        let main_body = function_body(&ir, "main").expect("main body");
        assert!(
            main_body.contains("call ptr @karac_runtime_tls_config_new("),
            "main should call _tls_config_new; body was:\n{}",
            main_body
        );
        assert!(
            main_body.contains("call i32 @karac_runtime_tls_listener_bind("),
            "main should call _tls_listener_bind; body was:\n{}",
            main_body
        );
    }

    #[test]
    fn test_ir_tls_listener_accept_parks_then_calls_runtime_ffi() {
        // `listener.accept()` parks via the canonical
        // `karac_park_on_fd` state-machine then calls
        // `karac_runtime_tls_accept(fd, config)`.
        let ir = ir_for(
            r#"
fn main() {
    let listener: TlsListener = TlsListener.bind_tls("127.0.0.1:0", "c", "k");
    let stream: TlsStream = listener.accept();
}
"#,
        );
        let main_body = function_body(&ir, "main").expect("main body");
        assert!(
            main_body.contains("@__kara_poll_karac_park_on_fd")
                || main_body.contains("kara.park.poll_wait"),
            "accept should compose via karac_park_on_fd; body was:\n{}",
            main_body
        );
        assert!(
            main_body.contains("call i32 @karac_runtime_tls_accept("),
            "accept should call _tls_accept; body was:\n{}",
            main_body
        );
    }

    #[test]
    fn test_ir_tls_stream_read_dispatches_to_runtime_ffi() {
        let ir = ir_for(
            r#"
fn handle(s: TlsStream) {
    let mut buf: Array[u8, 16] = [0u8; 16];
    let _ = s.read(mut buf);
}
fn main() {}
"#,
        );
        let body = function_body(&ir, "handle").expect("handle body");
        assert!(
            body.contains("call i64 @karac_runtime_tls_read("),
            "read should dispatch to _tls_read; body was:\n{}",
            body
        );
    }

    #[test]
    fn test_ir_tls_stream_write_dispatches_to_runtime_ffi() {
        let ir = ir_for(
            r#"
fn handle(s: TlsStream) {
    let msg: String = "hello";
    let _ = s.write(msg.bytes());
}
fn main() {}
"#,
        );
        let body = function_body(&ir, "handle").expect("handle body");
        assert!(
            body.contains("call i64 @karac_runtime_tls_write("),
            "write should dispatch to _tls_write; body was:\n{}",
            body
        );
    }

    #[test]
    fn test_ir_tls_stream_write_all_loops_via_runtime_ffi() {
        // write_all is a loop calling _tls_write — the IR should have
        // a `tls.wa.loop.head` BB and a call to _tls_write inside it.
        let ir = ir_for(
            r#"
fn handle(s: TlsStream) {
    let msg: String = "hello";
    let _ = s.write_all(msg.bytes());
}
fn main() {}
"#,
        );
        let body = function_body(&ir, "handle").expect("handle body");
        assert!(
            body.contains("tls.wa.loop.head"),
            "write_all should emit the labelled loop head; body was:\n{}",
            body
        );
        assert!(
            body.contains("call i64 @karac_runtime_tls_write("),
            "write_all loop should call _tls_write; body was:\n{}",
            body
        );
    }

    #[test]
    fn test_ir_tls_listener_drop_frees_config_then_closes_fd() {
        // A program that constructs a TlsListener and lets it drop at
        // scope exit should emit the `karac_drop_TlsListener` wrapper
        // calling into `@TlsListener.drop`, whose body frees the
        // config and closes the fd via the runtime FFIs.
        let ir = ir_for(
            r#"
fn main() {
    let listener: TlsListener = TlsListener.bind_tls("127.0.0.1:0", "c", "k");
}
"#,
        );
        assert!(
            ir.contains("@karac_drop_TlsListener"),
            "expected `@karac_drop_TlsListener` wrapper in IR"
        );
        assert!(
            ir.contains("@TlsListener.drop"),
            "expected `@TlsListener.drop` body in IR"
        );
        let drop_body = function_body(&ir, "TlsListener.drop")
            .unwrap_or_else(|| panic!("TlsListener.drop body not found"));
        assert!(
            drop_body.contains("call void @karac_runtime_tls_config_free("),
            "drop body should free the config; body was:\n{}",
            drop_body
        );
        assert!(
            drop_body.contains("call i32 @karac_runtime_tls_close("),
            "drop body should close the fd; body was:\n{}",
            drop_body
        );
    }

    #[test]
    fn test_ir_tls_stream_drop_closes_fd() {
        // `TlsStream` shares the `{i32 fd}` layout with `TcpStream`
        // but routes through `_tls_close` (not `_tcp_close`) so the
        // runtime can remove the per-fd session entry from the
        // `SESSIONS` registry before closing the underlying fd. The
        // drop test triggers via an explicit accept (`TlsStream`
        // values are otherwise only produced by `listener.accept()`).
        let ir = ir_for(
            r#"
fn main() {
    let listener: TlsListener = TlsListener.bind_tls("127.0.0.1:0", "c", "k");
    let stream: TlsStream = listener.accept();
}
"#,
        );
        assert!(
            ir.contains("@karac_drop_TlsStream"),
            "expected `@karac_drop_TlsStream` wrapper in IR"
        );
        let drop_body = function_body(&ir, "TlsStream.drop")
            .unwrap_or_else(|| panic!("TlsStream.drop body not found"));
        assert!(
            drop_body.contains("call i32 @karac_runtime_tls_close("),
            "TlsStream.drop should close fd via _tls_close; body was:\n{}",
            drop_body
        );
    }

    #[test]
    fn test_ir_tls_listener_by_value_param_uses_struct_type() {
        // Mirror of the slice-9 test for TcpListener: a user fn taking
        // `TlsListener` by value should get a `{ i32, ptr }` parameter
        // shape (matching the runtime-side struct shape) rather than
        // the i64 fall-through default. Surfaced by Demo 1 slice 2's
        // accept-loop pattern.
        let ir = ir_for(
            r#"
fn handle(l: TlsListener) {}
fn main() {}
"#,
        );
        assert!(
            ir.contains("define internal void @handle({ i32, ptr }"),
            "handle should take TlsListener as `{{ i32, ptr }}`; IR was:\n{}",
            ir
        );
    }

    #[test]
    fn test_ir_ws_accept_tls_runtime_ffi_declared() {
        // Phase 6 line 236 slice 3 — the WS-over-TLS accept FFI
        // declaration is emitted by `Codegen::new` unconditionally,
        // so any program (incl. trivial main) carries it.
        let ir = ir_for("fn main() {}");
        assert!(
            ir.contains("karac_runtime_ws_accept_tls"),
            "expected declaration of `karac_runtime_ws_accept_tls` in IR"
        );
    }

    #[test]
    fn test_ir_websocket_accept_tls_parks_then_calls_runtime_ffi() {
        // Phase 6 line 236 slice 3 — `WebSocket.accept_tls(listener)`
        // parks on listener-readability then calls into
        // `karac_runtime_ws_accept_tls(fd, config)`. The result is
        // a `WebSocket { fd }` struct value.
        let ir = ir_for(
            r#"
fn main() {
    let listener: TlsListener = TlsListener.bind_tls("127.0.0.1:0", "c", "k");
    let ws: WebSocket = WebSocket.accept_tls(listener);
}
"#,
        );
        let main_body = function_body(&ir, "main").expect("main body");
        assert!(
            main_body.contains("@__kara_poll_karac_park_on_fd")
                || main_body.contains("kara.park.poll_wait"),
            "accept_tls should park before the handshake; body was:\n{}",
            main_body
        );
        assert!(
            main_body.contains("call i32 @karac_runtime_ws_accept_tls("),
            "accept_tls should call _ws_accept_tls; body was:\n{}",
            main_body
        );
    }

    #[test]
    fn test_ir_websocket_accept_tls_returns_websocket_struct_shape() {
        // The lowering packs the conn fd into a `WebSocket { i32 }`
        // struct value via insert_value — same shape as the plain-
        // TCP `WebSocket.accept(listener)` lowering — so subsequent
        // `recv_text` / `send_text` / Drop dispatch lands on the
        // same WebSocket value-model branch.
        let ir = ir_for(
            r#"
fn main() {
    let listener: TlsListener = TlsListener.bind_tls("127.0.0.1:0", "c", "k");
    let ws: WebSocket = WebSocket.accept_tls(listener);
}
"#,
        );
        let main_body = function_body(&ir, "main").expect("main body");
        // `insertvalue` packing the i32 fd into the WebSocket struct.
        assert!(
            main_body.contains("insertvalue") && main_body.contains("ws.accept_tls.val"),
            "accept_tls should insertvalue the conn_fd into the WebSocket struct; \
             body was:\n{}",
            main_body
        );
    }

    #[test]
    fn test_ir_ws_recv_text_routes_through_runtime_ffi_for_tls_accept_result() {
        // The WS framing FFIs (`karac_runtime_ws_recv_text` /
        // `_send_text`) auto-dispatch through TLS at runtime by
        // looking up the fd in the TLS session registry. From a
        // codegen perspective the recv_text dispatch is unchanged
        // (same FFI symbol regardless of underlying transport) —
        // this test guards that the lowering still routes there
        // when the WebSocket came from `accept_tls`.
        let ir = ir_for(
            r#"
fn main() {
    let listener: TlsListener = TlsListener.bind_tls("127.0.0.1:0", "c", "k");
    let ws: WebSocket = WebSocket.accept_tls(listener);
    let mut buf: Array[u8, 64] = [0u8; 64];
    let _ = ws.recv_text(mut buf);
}
"#,
        );
        let main_body = function_body(&ir, "main").expect("main body");
        assert!(
            main_body.contains("call i64 @karac_runtime_ws_recv_text("),
            "recv_text should still dispatch to _ws_recv_text after accept_tls; \
             body was:\n{}",
            main_body
        );
    }

    #[test]
    fn test_ir_tls_stream_by_value_param_uses_struct_type() {
        let ir = ir_for(
            r#"
fn handle(s: TlsStream) {}
fn main() {}
"#,
        );
        assert!(
            ir.contains("define internal void @handle({ i32 }"),
            "handle should take TlsStream as `{{ i32 }}`; IR was:\n{}",
            ir
        );
    }
}
