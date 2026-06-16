//! Phase 6 line 17 slice 9e.1 — stdlib `WebSocket` codegen IR-grep tests.
//!
//! The wire-format correctness of `karac_runtime_ws_send_text` /
//! `_recv_text` is pinned by Rust unit tests inside the runtime
//! crate (`runtime/src/event_loop.rs` test module). This file
//! pins the *codegen-side* wiring: that kara source referencing
//! `WebSocket.send_text` / `.recv_text` / `Drop` lowers to the
//! right IR shape (park then runtime FFI, plus the
//! `@karac_drop_WebSocket` wrapper at scope exit).
//!
//! **Why not a kara-source E2E test of the framing round-trip?**
//! The kara-source surface for constructing a WebSocket from an
//! accepted TcpStream requires `stream.fd` field access. Today
//! that returns const 0 at codegen because `struct_types` is
//! only populated for user `program.items`, not stdlib bodies —
//! the field-access lowering falls back to a zero default. Filed
//! as a follow-on in phase-7-codegen.md; affects any stdlib-type
//! field access, not WebSocket-specific. Slice 9e.2's
//! `accept_websocket(listener)` builder bypasses the field-access
//! path and unlocks kara-source E2E coverage; slice 9e.1's
//! correctness gate is the runtime FFI tests + these IR-grep
//! tests for the codegen wiring.

#![cfg(feature = "llvm")]

mod ws_codegen_tests {
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

    /// Locate a function definition body by name; same shape as
    /// `tests/codegen.rs::function_body`.
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
    fn test_ir_websocket_drop_wrapper_emitted() {
        // A program that constructs a WebSocket via `from_fd` and
        // lets it drop at scope exit should emit the
        // `karac_drop_WebSocket` wrapper alongside the underlying
        // `@WebSocket.drop` body. The wrapper's body calls into
        // `@WebSocket.drop`, which (hand-rolled by
        // `emit_hardcoded_stdlib_drop_bodies` for stdlib types
        // sharing the single-i32-field layout) ultimately calls
        // `karac_runtime_tcp_close`.
        let ir = ir_for(
            r#"
fn main() {
    let ws = WebSocket.from_fd(3);
    println(ws.fd);
}
"#,
        );
        assert!(
            ir.contains("@karac_drop_WebSocket"),
            "expected `@karac_drop_WebSocket` wrapper in IR:\n{}",
            ir
        );
        assert!(
            ir.contains("@WebSocket.drop"),
            "expected hand-rolled `@WebSocket.drop` in IR:\n{}",
            ir
        );
        let drop_body = function_body(&ir, "WebSocket.drop")
            .unwrap_or_else(|| panic!("WebSocket.drop body not found:\n{}", ir));
        assert!(
            drop_body.contains("call i32 @karac_runtime_tcp_close("),
            "WebSocket.drop body should close the fd via \
             `karac_runtime_tcp_close`; body was:\n{}",
            drop_body
        );
    }

    #[test]
    fn test_ir_websocket_send_text_dispatches_to_runtime_ffi() {
        // `ws.send_text(buf)` should park on write-readiness then
        // call `karac_runtime_ws_send_text`. The Result wrapping
        // reuses the slice-9b `wrap_tcp_io_result` so labels show
        // `tcp.write.*` BB names — that's the cross-pollination
        // documented in `lower_websocket_io`.
        let ir = ir_for(
            r#"
fn main() {
    let ws = WebSocket.from_fd(3);
    let msg: String = "hi";
    let _ = ws.send_text(msg.bytes());
}
"#,
        );
        let main_body =
            function_body(&ir, "main").unwrap_or_else(|| panic!("main body not found:\n{}", ir));
        assert!(
            main_body.contains("call i64 @karac_runtime_ws_send_text("),
            "main should call `karac_runtime_ws_send_text`; body was:\n{}",
            main_body
        );
        // Park primitive must precede the FFI call (slice 6 +
        // slice 9 compose-at-leaf shape). Check that the park
        // poll state machine is present in main.
        assert!(
            main_body.contains("__kara_poll_karac_park_on_fd"),
            "main should park before send_text; body was:\n{}",
            main_body
        );
    }

    #[test]
    fn test_ir_websocket_recv_text_dispatches_to_runtime_ffi() {
        let ir = ir_for(
            r#"
fn main() {
    let ws = WebSocket.from_fd(3);
    let mut buf: Array[u8, 64] = [0u8; 64];
    let _ = ws.recv_text(mut buf);
}
"#,
        );
        let main_body =
            function_body(&ir, "main").unwrap_or_else(|| panic!("main body not found:\n{}", ir));
        assert!(
            main_body.contains("call i64 @karac_runtime_ws_recv_text("),
            "main should call `karac_runtime_ws_recv_text`; body was:\n{}",
            main_body
        );
        assert!(
            main_body.contains("__kara_poll_karac_park_on_fd"),
            "main should park before recv_text; body was:\n{}",
            main_body
        );
    }

    #[test]
    fn test_ir_websocket_accept_parks_then_calls_runtime_ffi() {
        // Slice 9e.2 — `WebSocket.accept(listener)` should park on
        // listener-readability, then call `karac_runtime_ws_accept`
        // which performs accept + HTTP upgrade. The returned i32
        // gets packed into a `WebSocket { fd }` struct value.
        let ir = ir_for(
            r#"
fn main() {
    let listener = TcpListener.bind("127.0.0.1:0").unwrap();
    let ws = WebSocket.accept(listener).unwrap();
    println(ws.fd);
}
"#,
        );
        let main_body =
            function_body(&ir, "main").unwrap_or_else(|| panic!("main body not found:\n{}", ir));
        assert!(
            main_body.contains("call i64 @karac_runtime_ws_accept("),
            "main should call `karac_runtime_ws_accept`; body was:\n{}",
            main_body
        );
        // Park primitive must precede the FFI call.
        assert!(
            main_body.contains("__kara_poll_karac_park_on_fd"),
            "main should park before ws_accept; body was:\n{}",
            main_body
        );
    }

    // ── Phase 6 line 17 slice 9e.3 — binary frame dispatch ────────

    #[test]
    fn test_ir_websocket_send_binary_dispatches_to_runtime_ffi() {
        let ir = ir_for(
            r#"
fn main() {
    let ws = WebSocket.from_fd(3);
    let msg: String = "bin";
    let _ = ws.send_binary(msg.bytes());
}
"#,
        );
        let main_body =
            function_body(&ir, "main").unwrap_or_else(|| panic!("main body not found:\n{}", ir));
        assert!(
            main_body.contains("call i64 @karac_runtime_ws_send_binary("),
            "main should call `karac_runtime_ws_send_binary`; body was:\n{}",
            main_body
        );
        assert!(
            main_body.contains("__kara_poll_karac_park_on_fd"),
            "main should park before send_binary; body was:\n{}",
            main_body
        );
    }

    #[test]
    fn test_ir_websocket_recv_binary_dispatches_to_runtime_ffi() {
        let ir = ir_for(
            r#"
fn main() {
    let ws = WebSocket.from_fd(3);
    let mut buf: Array[u8, 64] = [0u8; 64];
    let _ = ws.recv_binary(mut buf);
}
"#,
        );
        let main_body =
            function_body(&ir, "main").unwrap_or_else(|| panic!("main body not found:\n{}", ir));
        assert!(
            main_body.contains("call i64 @karac_runtime_ws_recv_binary("),
            "main should call `karac_runtime_ws_recv_binary`; body was:\n{}",
            main_body
        );
        assert!(
            main_body.contains("__kara_poll_karac_park_on_fd"),
            "main should park before recv_binary; body was:\n{}",
            main_body
        );
    }

    // ── Phase 6 line 17 slice 9e.4 — client-side masked send ────────

    #[test]
    fn test_ir_websocket_send_text_masked_dispatches_to_runtime_ffi() {
        let ir = ir_for(
            r#"
fn main() {
    let ws = WebSocket.from_fd(3);
    let msg: String = "client-msg";
    let _ = ws.send_text_masked(msg.bytes());
}
"#,
        );
        let main_body =
            function_body(&ir, "main").unwrap_or_else(|| panic!("main body not found:\n{}", ir));
        assert!(
            main_body.contains("call i64 @karac_runtime_ws_send_text_masked("),
            "main should call `karac_runtime_ws_send_text_masked`; body was:\n{}",
            main_body
        );
        assert!(
            main_body.contains("__kara_poll_karac_park_on_fd"),
            "main should park before send_text_masked; body was:\n{}",
            main_body
        );
    }

    #[test]
    fn test_ir_websocket_send_binary_masked_dispatches_to_runtime_ffi() {
        let ir = ir_for(
            r#"
fn main() {
    let ws = WebSocket.from_fd(3);
    let msg: String = "bin";
    let _ = ws.send_binary_masked(msg.bytes());
}
"#,
        );
        let main_body =
            function_body(&ir, "main").unwrap_or_else(|| panic!("main body not found:\n{}", ir));
        assert!(
            main_body.contains("call i64 @karac_runtime_ws_send_binary_masked("),
            "main should call `karac_runtime_ws_send_binary_masked`; body was:\n{}",
            main_body
        );
    }

    #[test]
    fn test_ir_websocket_masked_send_ffis_declared() {
        let ir = ir_for("fn main() {}");
        assert!(
            ir.contains("declare i64 @karac_runtime_ws_send_text_masked(i64, ptr, i64)"),
            "expected ws_send_text_masked FFI declaration; IR:\n{}",
            ir
        );
        assert!(
            ir.contains("declare i64 @karac_runtime_ws_send_binary_masked(i64, ptr, i64)"),
            "expected ws_send_binary_masked FFI declaration; IR:\n{}",
            ir
        );
    }

    #[test]
    fn test_ir_websocket_binary_ffis_declared() {
        let ir = ir_for("fn main() {}");
        assert!(
            ir.contains("declare i64 @karac_runtime_ws_send_binary(i64, ptr, i64)"),
            "expected ws_send_binary FFI declaration; IR:\n{}",
            ir
        );
        assert!(
            ir.contains("declare i64 @karac_runtime_ws_recv_binary(i64, ptr, i64)"),
            "expected ws_recv_binary FFI declaration; IR:\n{}",
            ir
        );
    }

    #[test]
    fn test_ir_websocket_accept_ffi_declared() {
        // Defensive: the accept FFI declaration lands
        // unconditionally in `Codegen::new`, alongside the framing
        // FFIs from slice 9e.1.
        let ir = ir_for("fn main() {}");
        assert!(
            ir.contains("declare i64 @karac_runtime_ws_accept(i64)"),
            "expected ws_accept FFI declaration; IR:\n{}",
            ir
        );
    }

    // ── Stdlib struct-as-by-value-param LLVM ABI regression guard ──
    //
    // The `TcpListener` / `TcpStream` / `WebSocket` baked-stdlib structs
    // share the `{ fd: i64 }` shape. `src/codegen/types_lowering.rs::
    // llvm_type_for_name` carries explicit arms for each so that a user
    // fn taking one by value gets the right LLVM signature shape rather
    // than the i64 fall-through default that produced the LLVM verifier
    // rejection surfaced by Demo 1 (line 170) slice 1's accept-loop.
    // The three tests below pin the function-signature shape end-to-end
    // through codegen.

    #[test]
    fn test_ir_websocket_by_value_param_uses_struct_type() {
        // A user fn taking `WebSocket` by value must declare its
        // parameter as `{ i64 }`, matching the value-site lowering of
        // `WebSocket.from_fd(_)` / `WebSocket.accept(_)`. Pre-fix the
        // declared param was `i64` (from the fall-through default in
        // `llvm_type_for_name`) and any direct `handle(ws)` call hit
        // an LLVM verifier rejection on `Call parameter type does not
        // match function signature`.
        let ir = ir_for(
            r#"
fn handle(ws: WebSocket) {}
fn main() {
    let ws = WebSocket.from_fd(3);
    handle(ws);
}
"#,
        );
        assert!(
            ir.contains("define internal void @handle({ i64 }"),
            "expected `@handle` to declare its WebSocket param as `{{ i64 }}`; IR:\n{}",
            ir
        );
    }

    #[test]
    fn test_ir_tcpstream_by_value_param_uses_struct_type() {
        let ir = ir_for(
            r#"
fn handle(s: TcpStream) {}
fn main() {
    let listener = TcpListener.bind("127.0.0.1:0").unwrap();
    let s = listener.accept().unwrap();
    handle(s);
}
"#,
        );
        assert!(
            ir.contains("define internal void @handle({ i64 }"),
            "expected `@handle` to declare its TcpStream param as `{{ i64 }}`; IR:\n{}",
            ir
        );
    }

    #[test]
    fn test_ir_tcplistener_by_value_param_uses_struct_type() {
        let ir = ir_for(
            r#"
fn handle(l: TcpListener) {}
fn main() {
    let listener = TcpListener.bind("127.0.0.1:0").unwrap();
    handle(listener);
}
"#,
        );
        assert!(
            ir.contains("define internal void @handle({ i64 }"),
            "expected `@handle` to declare its TcpListener param as `{{ i64 }}`; IR:\n{}",
            ir
        );
    }

    #[test]
    fn test_ir_websocket_runtime_ffis_declared() {
        // Sanity check that both runtime FFI declarations land in
        // the module's external-declaration section, even if no
        // user code calls them (defensive — `Codegen::new` adds
        // the declarations unconditionally so the FFI ABI is
        // stable for any kara program that might bring in
        // WebSocket).
        let ir = ir_for("fn main() {}");
        assert!(
            ir.contains("declare i64 @karac_runtime_ws_send_text(i64, ptr, i64)"),
            "expected ws_send_text FFI declaration; IR:\n{}",
            ir
        );
        assert!(
            ir.contains("declare i64 @karac_runtime_ws_recv_text(i64, ptr, i64)"),
            "expected ws_recv_text FFI declaration; IR:\n{}",
            ir
        );
    }
}
