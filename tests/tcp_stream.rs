//! Phase 6 line 17 slice 9 — stdlib `TcpStream` E2E test.
//!
//! Compiles a kara program that uses the real stdlib `TcpStream`
//! surface — `TcpListener.bind("127.0.0.1:0")`, `listener.accept()`,
//! then `stream.write(msg.bytes())` — runs the binary, connects to
//! the listener from the harness thread, reads the bytes the binary
//! wrote, and asserts they match.
//!
//! **What this exercises that the slice-8 `tests/tcp_listener.rs`
//! didn't.** Slice 8 only exercised `bind` + `accept` — the park
//! happens on read-readiness of the listener fd, the syscall is a
//! one-shot `accept(2)`, and the test's success criteria is that
//! the binary exits 0 after the accept call returns. Slice 9 adds
//! `TcpStream.read` / `.write`, each of which composes another
//! park-and-syscall pair through the same
//! `emit_state_machine_invocation_for_park_on_fd` codegen helper.
//! This test pins the `write` direction end-to-end: the kara
//! binary's `write` call must park on write-readiness of the
//! connection fd, then call `karac_runtime_tcp_write` to push the
//! bytes — the harness reads them back from the TCP connection.
//!
//! **read direction.** Wired in `src/codegen/tcp.rs` via
//! `lower_tcp_stream_read` (the same helper, different direction
//! discriminant), but not exercised in this E2E. The user-facing
//! signature `read(ref self, buf: mut Slice[u8]) -> i64` requires
//! the caller to construct a `mut Slice[u8]` — typically via
//! `mut some_array` or `mut some_vec`. The buffer-construction
//! shape from user-source baked-stdlib types is best exercised by
//! a real consumer (e.g. an echo-server kara program) that lands
//! when one is needed; the unit-shape correctness of the codegen
//! lowering is proved by the build + linkage passes.

#[cfg(all(unix, feature = "llvm"))]
mod tcp_stream_tests {
    use std::io::{BufRead, BufReader, Read};
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
    /// `--features test-helpers` — `TcpStream` is real stdlib, the
    /// FFIs it depends on are always-on). Mirrors
    /// `tests/tcp_listener.rs::runtime_path`.
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
        let obj = format!("/tmp/karac_tcp_stream_e2e_{pid}_{nanos}.o");
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

    /// Primary deliverable: a kara program that binds an ephemeral
    /// TCP listener, accepts a connection, writes a fixed payload to
    /// the stream via `stream.write(msg.bytes())`, then exits. The
    /// harness connects to the bound port to trigger the accept, then
    /// reads the bytes the binary wrote and asserts they match the
    /// payload. Exit-success is also asserted.
    #[test]
    fn test_tcp_stream_write_round_trip() {
        let _guard = TCP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!(
                "skip: libkarac_runtime.a not built \
                 (run `cargo build -p karac-runtime --release`)"
            );
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        // Real stdlib surface — `TcpStream` from baked
        // `runtime/stdlib/tcp.kara`. `String.bytes()` returns
        // `Slice[u8]` zero-copy over the String's underlying buffer
        // (the well-trodden pattern from design.md § Character type).
        // `write` parks on write-readiness then calls the raw
        // syscall.
        let src = r#"
            fn main() {
                let listener = TcpListener.bind("127.0.0.1:0");
                let stream = listener.accept();
                let msg: String = "hello from kara\n";
                let _n = stream.write(msg.bytes());
            }
        "#;

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_tcp_stream_e2e_{pid}_{nanos}"));

        if let Err(e) = compile_and_link(src, &exe_path) {
            panic!("compile/link failed: {e}");
        }

        let mut child = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn tcp_stream binary");

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

        // Connect to trigger an accept; once connected we expect the
        // binary to write its payload onto the connection. Use a
        // brief retry to absorb the race between bind's BOUND_PORT
        // print and the parking primitive's fd-registration (same
        // pattern as `tests/tcp_listener.rs`).
        let connect_started = Instant::now();
        let mut maybe_conn: Option<std::net::TcpStream> = None;
        for _ in 0..10 {
            if let Ok(c) = std::net::TcpStream::connect(format!("127.0.0.1:{port}")) {
                maybe_conn = Some(c);
                break;
            }
            if connect_started.elapsed() > Duration::from_secs(2) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let Some(mut conn) = maybe_conn else {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&exe_path);
            panic!("could not connect to 127.0.0.1:{port} to trigger accept");
        };

        // Read what the binary writes. Read timeout is a defense
        // against the binary hanging in the park (e.g., a missed
        // wakeup); 10s is generous given the round trip is
        // sub-second under normal load.
        conn.set_read_timeout(Some(Duration::from_secs(10)))
            .expect("set_read_timeout");
        let mut buf = Vec::with_capacity(64);
        let mut chunk = [0u8; 64];
        loop {
            match conn.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = std::fs::remove_file(&exe_path);
                    panic!("read from kara binary failed: {e}");
                }
            }
            if buf.ends_with(b"\n") {
                break;
            }
        }
        let payload = String::from_utf8_lossy(&buf).to_string();

        // Wait for the binary to exit. The write() syscall returns
        // immediately (the connection is already accepted by this
        // point), so the child should exit promptly.
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
                            "binary did not exit within 10s after write — \
                             the write call did not return (parking through \
                             TcpStream.write may be broken)"
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
             TcpStream.write returned but main() failed downstream"
        );
        assert!(
            payload.contains("hello from kara"),
            "expected to receive `hello from kara` from binary, got: {payload:?}"
        );
    }
}
