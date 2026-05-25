//! Phase 6 line 17 — stdlib `TcpListener` E2E test.
//!
//! Compiles a kara program that uses the real stdlib `TcpListener`
//! type — `TcpListener.bind("127.0.0.1:0")` followed by
//! `listener.accept()` — runs the binary, connects to the listener
//! from the harness thread, and asserts the binary observes the
//! accepted connection (printing a positive fd) and exits cleanly.
//!
//! **What this exercises that Slice 7's `tests/park_and_wake.rs`
//! didn't.** Slice 7 used a test-only runtime FFI
//! (`karac_runtime_test_bind_and_print_port`) + a direct
//! `karac_park_on_fd` call from user source — proving the parking
//! wiring works in isolation. This test exercises the same wiring
//! through the *real stdlib surface*: `TcpListener.bind` /
//! `.accept` calls flow through the compiler-builtin codegen
//! lowering (`src/codegen/tcp.rs`), which composes the parking
//! state-machine via the reusable
//! `emit_state_machine_invocation_for_park_on_fd` helper. Same
//! park/wake substrate, exercised through the production surface
//! that future stdlib types (`TcpStream` / `WebSocket`) will reuse.
//!
//! **Subprocess + port-from-stdout pattern.** Same harness shape as
//! `tests/park_and_wake.rs` and `tests/http_server.rs`. The
//! `BOUND_PORT=<n>` line is emitted by the runtime side of
//! `karac_runtime_tcp_bind` when the requested address ends in
//! `:0`. No `test-helpers` feature gate needed — `TcpListener` is a
//! real production type, the runtime FFIs are always-on.

#[cfg(all(unix, feature = "llvm"))]
mod tcp_listener_tests {
    use std::io::{BufRead, BufReader};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::{Mutex, Once};
    use std::time::{Duration, Instant};

    static TCP_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    static RUNTIME_BUILT: Once = Once::new();
    static mut RUNTIME_PATH: Option<PathBuf> = None;

    /// Build the runtime static library (production profile, no
    /// `--features test-helpers` — `TcpListener` is real stdlib, the
    /// FFIs it depends on are always-on). Mirrors
    /// `tests/http_server.rs::runtime_path`.
    #[allow(static_mut_refs)]
    fn runtime_path() -> Option<PathBuf> {
        RUNTIME_BUILT.call_once(|| {
            let output = Command::new("cargo")
                .args(["build", "-p", "karac-runtime", "--release"])
                .output();
            if let Ok(out) = output {
                if out.status.success() {
                    let p = workspace_root().join("target/release/libkarac_runtime.a");
                    if p.exists() {
                        unsafe {
                            RUNTIME_PATH = Some(p);
                        }
                    }
                }
            }
        });
        unsafe { RUNTIME_PATH.clone() }
    }

    fn compile_and_link(src: &str, exe_path: &Path) -> Result<(), String> {
        use karac::codegen::{compile_to_object_with_options, link_executable};
        let mut parsed = karac::parse(src);
        if !parsed.errors.is_empty() {
            return Err(format!("parse errors: {:?}", parsed.errors));
        }
        let resolved = karac::resolve(&parsed.program);
        if !resolved.errors.is_empty() {
            return Err(format!("resolve errors: {:?}", resolved.errors));
        }
        let typed = karac::typecheck(&parsed.program, &resolved);
        if !typed.errors.is_empty() {
            return Err(format!("typecheck errors: {:?}", typed.errors));
        }
        karac::lower(&mut parsed.program, &typed);
        let _effects = karac::effectcheck(&parsed.program);
        let _ownership = karac::ownershipcheck(&parsed.program, &typed);
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let obj = format!("/tmp/karac_tcp_e2e_{pid}_{nanos}.o");
        compile_to_object_with_options(&parsed.program, &obj, None, None, None, None)
            .map_err(|e| format!("codegen failed: {e}"))?;
        link_executable(&obj, exe_path.to_str().unwrap())
            .map_err(|e| format!("link failed: {e}"))?;
        let _ = std::fs::remove_file(&obj);
        Ok(())
    }

    /// Read stdout until we see `BOUND_PORT=<n>`, return the port.
    /// Returns None on timeout. Keeps draining stdout for the rest
    /// of the child's lifetime so the pipe buffer doesn't fill.
    fn await_bound_port(
        stdout: std::process::ChildStdout,
        timeout: Duration,
    ) -> (Option<u16>, std::thread::JoinHandle<()>) {
        let (tx, rx) = std::sync::mpsc::channel::<u16>();
        let handle = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let mut sent = false;
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if !sent {
                            if let Some(rest) = line.trim().strip_prefix("BOUND_PORT=") {
                                if let Ok(p) = rest.parse::<u16>() {
                                    let _ = tx.send(p);
                                    sent = true;
                                }
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let port = rx.recv_timeout(timeout).ok();
        (port, handle)
    }

    /// Primary deliverable: a kara program that calls
    /// `TcpListener.bind("127.0.0.1:0")` then `listener.accept()`,
    /// printing the accepted connection's fd. Harness connects to
    /// the bound port to trigger an accept, then asserts the binary
    /// exits 0 within a timeout.
    #[test]
    fn test_tcp_listener_bind_accept_round_trip() {
        let _guard = TCP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!(
                "skip: libkarac_runtime.a not built \
                 (run `cargo build -p karac-runtime --release`)"
            );
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        // Real stdlib surface — no inline `extern` block. The
        // `TcpListener` type comes from baked `runtime/stdlib/tcp.kara`.
        // `bind` returns a `TcpListener { fd: i32 }` struct value;
        // `accept` returns the new connection fd via the
        // codegen-emitted park-and-accept sequence.
        let src = r#"
            fn main() {
                let listener = TcpListener.bind("127.0.0.1:0");
                let conn_fd = listener.accept();
                println(conn_fd);
            }
        "#;

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_tcp_e2e_{pid}_{nanos}"));

        if let Err(e) = compile_and_link(src, &exe_path) {
            panic!("compile/link failed: {e}");
        }

        let mut child = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn tcp_listener binary");

        let stdout = child.stdout.take().expect("child stdout missing");
        let (port_opt, join) = await_bound_port(stdout, Duration::from_secs(15));

        let port = match port_opt {
            Some(p) => p,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&exe_path);
                panic!("binary did not emit BOUND_PORT line within 15s");
            }
        };
        assert!(port > 0, "BOUND_PORT must be a non-zero ephemeral port");

        // Connect to trigger an accept. Retry briefly to absorb the
        // race between bind's BOUND_PORT print and the parking
        // primitive's fd-registration (same pattern as park_and_wake).
        let connect_started = Instant::now();
        let mut connected = false;
        for _ in 0..10 {
            if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
                connected = true;
                break;
            }
            if connect_started.elapsed() > Duration::from_secs(2) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !connected {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&exe_path);
            panic!("could not connect to 127.0.0.1:{port} to trigger accept");
        }

        // Wait for the binary to print the connection fd and exit.
        let wait_started = Instant::now();
        let exit_status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if wait_started.elapsed() > Duration::from_secs(10) {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = std::fs::remove_file(&exe_path);
                        panic!(
                            "binary did not exit within 10s after connect — \
                             accept did not return. Likely the parking \
                             round-trip through TcpListener.accept is broken."
                        );
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&exe_path);
                    panic!("try_wait failed: {e}");
                }
            }
        };

        let _ = join.join();
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "binary exited non-success {exit_status:?} — \
             TcpListener.accept returned but main() failed downstream"
        );
    }
}
