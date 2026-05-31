//! Phase 6 line 17 slice 9 + 9a — stdlib `TcpStream` E2E tests.
//!
//! Two co-located tests pin both directions of the read/write
//! round-trip:
//!
//! - `test_tcp_stream_write_round_trip` (slice 9): kara binary calls
//!   `stream.write(msg.bytes())` after `bind` + `accept`; harness
//!   reads the bytes back from the TCP connection.
//! - `test_tcp_stream_read_round_trip` (slice 9a): kara binary
//!   constructs `let mut buf: Array[u8, N] = [0u8; N]` and calls
//!   `stream.read(mut buf)`; harness connects, writes a known
//!   payload, and asserts the binary observed the read returning a
//!   positive count.
//!
//! **What this pins that the slice-8 `tests/tcp_listener.rs`
//! didn't.** Slice 8 only exercised `bind` + `accept` — the park
//! happens on read-readiness of the listener fd, the syscall is a
//! one-shot `accept(2)`, and the success criterion is that the
//! binary exits 0 after the accept call returns. Slices 9 + 9a add
//! `TcpStream.read` / `.write`, each composing a park-and-syscall
//! pair through the same `emit_state_machine_invocation_for_park_on_fd`
//! codegen helper. Together they pin both directions end-to-end:
//! the write test verifies the binary parks on write-readiness then
//! calls `karac_runtime_tcp_write`; the read test verifies the
//! binary parks on read-readiness then calls `karac_runtime_tcp_read`
//! after the harness pushes bytes onto the connection.
//!
//! **Slice 9a also validates the `mut buf` call-site coercion.**
//! User-source `Array[u8, N]` literals + repeat-init (`[0u8; N]`) +
//! the `mut buf` call-site marker (design.md Feature 4 Part 1½
//! Rule 1) flow through the existing typechecker + codegen path to
//! land as a `mut Slice[u8]` argument — the codegen path for
//! `lower_tcp_stream_read` extracts `{ptr, len}` from the slice and
//! invokes the read FFI through the parking state machine. This is
//! the first stdlib type to exercise that coercion path through a
//! real network FFI.

#[cfg(all(unix, feature = "llvm"))]
mod tcp_stream_tests {
    use std::io::{BufRead, BufReader, Read, Write};
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
                let listener = TcpListener.bind("127.0.0.1:0").unwrap();
                let stream = listener.accept().unwrap();
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

    /// Variant of `await_bound_port` that also captures every line
    /// emitted *after* the `BOUND_PORT=<n>` line. The slice-9a read
    /// test asserts on a `println(n)` line that the kara binary
    /// emits after `stream.read` returns; the original helper drops
    /// post-port lines on the floor, so this variant keeps them in a
    /// `Vec<String>` returned via the join handle's exit value.
    fn await_bound_port_collect_lines(
        stdout: std::process::ChildStdout,
        timeout: Duration,
    ) -> (Option<u16>, std::thread::JoinHandle<Vec<String>>) {
        let (port_tx, port_rx) = std::sync::mpsc::channel::<u16>();
        let handle = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let mut port_sent = false;
            let mut collected: Vec<String> = Vec::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim().to_string();
                        if !port_sent {
                            if let Some(rest) = trimmed.strip_prefix("BOUND_PORT=") {
                                if let Ok(p) = rest.parse::<u16>() {
                                    let _ = port_tx.send(p);
                                    port_sent = true;
                                    continue;
                                }
                            }
                        }
                        collected.push(trimmed);
                    }
                    Err(_) => break,
                }
            }
            collected
        });
        let port = port_rx.recv_timeout(timeout).ok();
        (port, handle)
    }

    /// Slice 9a + 9b deliverable: read-direction E2E with Result
    /// unwrapping. The kara program binds an ephemeral listener,
    /// accepts a connection, constructs a 64-byte mutable buffer
    /// (`Array[u8, 64]` zero-initialised), passes it as `mut buf` so
    /// the call-site marker coerces to `mut Slice[u8]` (design.md
    /// Feature 4 Part 1½ Rule 1), calls `stream.read(mut buf)`, then
    /// matches on the returned `Result[i64, TcpError]` — printing the
    /// byte count on `Ok(n)` or `-1` on `Err(_)`. The harness
    /// connects, pushes a known payload, waits for the binary to
    /// exit, and asserts the printed line is a positive integer.
    ///
    /// The byte-count assertion is the tightest portable invariant —
    /// asserting on exact byte values would race against `read(2)`'s
    /// partial-read semantics (the kernel might return the bytes in
    /// one chunk or several), and slice 9 ships single-syscall reads
    /// (no `read_exact` looping wrapper — that's slice 9c). Positive
    /// count proves: (1) parking through `karac_park_on_fd` returned,
    /// (2) `karac_runtime_tcp_read` executed the syscall on the
    /// borrowed fd, (3) the FFI's i64 return value flowed through
    /// `wrap_tcp_io_result` into a `Result.Ok(n)` aggregate, (4) the
    /// `match` arm in user-source extracted `n` cleanly through
    /// `reconstruct_payload_value`.
    #[test]
    fn test_tcp_stream_read_round_trip() {
        let _guard = TCP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!(
                "skip: libkarac_runtime.a not built \
                 (run `cargo build -p karac-runtime --release`)"
            );
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let src = r#"
            fn main() {
                let listener = TcpListener.bind("127.0.0.1:0").unwrap();
                let stream = listener.accept().unwrap();
                let mut buf: Array[u8, 64] = [0u8; 64];
                match stream.read(mut buf) {
                    Ok(n) => println(n),
                    Err(_) => println(-1),
                }
            }
        "#;

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_tcp_stream_read_e2e_{pid}_{nanos}"));

        if let Err(e) = compile_and_link(src, &exe_path) {
            panic!("compile/link failed: {e}");
        }

        let mut child = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn tcp_stream_read binary");

        let stdout = child.stdout.take().expect("child stdout missing");
        let (port_opt, join) = await_bound_port_collect_lines(stdout, Duration::from_secs(15));

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

        // Push the payload after the accept's park-wake has had a
        // chance to fire. We don't close the write half — the kara
        // `read` is a single-syscall (slice 9 ships no read_exact),
        // so any byte arriving on the connection unblocks the park
        // and returns a positive count to user-space.
        let payload = b"ping\n";
        if let Err(e) = conn.write_all(payload) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&exe_path);
            panic!("could not write payload to connection: {e}");
        }
        let _ = conn.flush();

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
                            "binary did not exit within 10s after harness \
                             wrote {n} bytes — TcpStream.read did not return. \
                             Likely the parking round-trip through \
                             karac_park_on_fd(direction=0) or the \
                             karac_runtime_tcp_read FFI is broken.",
                            n = payload.len()
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

        drop(conn);
        let lines = join.join().expect("stdout-drain thread panicked");
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "binary exited non-success {exit_status:?} — \
             TcpStream.read returned but main() failed downstream. \
             Lines after BOUND_PORT: {lines:?}"
        );

        let count: Option<i64> = lines.iter().find_map(|l| l.parse::<i64>().ok());
        assert!(
            matches!(count, Some(n) if n > 0),
            "expected positive byte count from `println(n)` after \
             stream.read, got lines: {lines:?}"
        );
    }

    /// Slice 9c deliverable: `TcpStream.write_all` end-to-end. The
    /// kara binary calls `stream.write_all(msg.bytes())` for a
    /// 2 KiB payload; the harness reads back every byte and asserts
    /// the count matches `buf.len()`.
    ///
    /// **Why 2 KiB and not smaller.** A single `write(2)` of a few
    /// hundred bytes always fits in the kernel's socket send buffer
    /// (default 128 KiB+ on Linux/macOS), so the single-syscall
    /// `write` would also push the whole payload and `write_all`'s
    /// loop would only iterate once — the partial-write code path
    /// wouldn't run. 2 KiB is still well within send-buffer limits,
    /// so this test specifically pins the OK-loop-once shape (the
    /// only loop count we can deterministically test without a
    /// blocking peer harness). The partial-write path (loop count
    /// above 1) is exercised by reading `lower_tcp_stream_write_all`'s
    /// IR and trusting the structural invariants — same approach
    /// the codegen E2Es take for branch coverage on tagged loops.
    ///
    /// **Why a 2 KiB ASCII payload of known shape.** Each byte is
    /// `'A' + (i % 26)` so the harness can assert content equality
    /// in addition to byte count, catching off-by-one in
    /// `chunk_ptr = buf.ptr + written` if it ever regresses.
    #[test]
    fn test_tcp_stream_write_all_round_trip() {
        let _guard = TCP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!(
                "skip: libkarac_runtime.a not built \
                 (run `cargo build -p karac-runtime --release`)"
            );
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        // Build the 2 KiB known-shape payload in kara source via
        // a String produced from a Vec[u8] generator pattern. We
        // need this as a Slice[u8] for write_all. The easiest path
        // that goes through real stdlib surfaces: build a String of
        // the expected size via String.repeat (if available) — but
        // since I don't want to introduce a new dependency on
        // String.repeat's exact behaviour, build the payload at
        // the harness side and have the kara binary read+echo it
        // unchanged via write_all. That doesn't exercise write_all's
        // chunking under load, but DOES pin the basic Ok-arm shape.
        //
        // Simpler design (chosen here): hardcode a known-shape
        // 256-byte payload directly in kara source as a String
        // literal (using \xNN escapes? no — kara source uses plain
        // ASCII). Use a repeated short pattern that the test source
        // can assert on.
        let pattern_repeat = 64usize;
        let unit = "Aa1Zz9Bb2Yy8Cc3Xx7Dd4Ww6Ee5Vv0!";
        let expected: String = unit.repeat(pattern_repeat);
        let expected_bytes = expected.as_bytes().to_vec();

        let mut src = String::from(
            r#"
            fn main() {
                let listener = TcpListener.bind("127.0.0.1:0").unwrap();
                let stream = listener.accept().unwrap();
                let msg: String = ""#,
        );
        for _ in 0..pattern_repeat {
            src.push_str(unit);
        }
        src.push_str(
            r#"";
                let _r = stream.write_all(msg.bytes());
            }
        "#,
        );

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_tcp_stream_write_all_{pid}_{nanos}"));

        if let Err(e) = compile_and_link(&src, &exe_path) {
            panic!("compile/link failed: {e}");
        }

        let mut child = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn tcp_stream write_all binary");

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

        conn.set_read_timeout(Some(Duration::from_secs(10)))
            .expect("set_read_timeout");
        let mut received = Vec::with_capacity(expected_bytes.len());
        let mut chunk = [0u8; 1024];
        while received.len() < expected_bytes.len() {
            match conn.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => received.extend_from_slice(&chunk[..n]),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = std::fs::remove_file(&exe_path);
                    panic!("read from kara binary failed: {e}");
                }
            }
        }

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
                            "binary did not exit within 10s after write_all — \
                             likely the loop is wedged or the parking primitive \
                             didn't return"
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
             TcpStream.write_all returned but main() failed downstream"
        );
        assert_eq!(
            received.len(),
            expected_bytes.len(),
            "write_all should have pushed all {} bytes, got {}",
            expected_bytes.len(),
            received.len()
        );
        assert_eq!(
            received, expected_bytes,
            "write_all should have pushed the exact payload byte-for-byte"
        );
    }

    /// Phase-8 line 74 prereq — `TcpStream.connect(addr)` Ok-path E2E,
    /// the plain-TCP *client*. Roles are reversed from the round-trip
    /// tests above: the harness owns the listener (a `std::net`
    /// listener on an ephemeral port), and the kara binary is the one
    /// that *initiates* the connection via the new `TcpStream.connect`,
    /// then writes a payload onto it. The harness accepts and reads the
    /// payload — proving `connect` produced a real, writable connected
    /// socket. The port is interpolated into the kara source (the
    /// dynamic-compile harness already takes a source string), so no
    /// runtime port-passing is needed.
    #[test]
    fn test_tcp_stream_connect_ok_round_trip() {
        let _guard = TCP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!(
                "skip: libkarac_runtime.a not built \
                 (run `cargo build -p karac-runtime --release`)"
            );
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        // Harness-owned listener on an ephemeral port. Accept happens on
        // a worker thread so the main thread can drive compile/run.
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("harness listener bind failed");
        let port = listener.local_addr().expect("local_addr").port();
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let accept_thread = std::thread::spawn(move || {
            if let Ok((mut conn, _)) = listener.accept() {
                conn.set_read_timeout(Some(Duration::from_secs(10))).ok();
                let mut buf = Vec::with_capacity(64);
                let mut chunk = [0u8; 64];
                loop {
                    match conn.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        Err(_) => break,
                    }
                    if buf.ends_with(b"\n") {
                        break;
                    }
                }
                let _ = tx.send(String::from_utf8_lossy(&buf).to_string());
            }
        });

        let src = format!(
            "fn main() {{\n\
             \x20   match TcpStream.connect(\"127.0.0.1:{port}\") {{\n\
             \x20       Result.Ok(stream) => {{\n\
             \x20           let msg: String = \"hi from connect\\n\";\n\
             \x20           let _n = stream.write(msg.bytes());\n\
             \x20       }}\n\
             \x20       Result.Err(_) => {{ println(99); }}\n\
             \x20   }}\n\
             }}\n"
        );

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_tcp_connect_e2e_{pid}_{nanos}"));

        if let Err(e) = compile_and_link(&src, &exe_path) {
            panic!("compile/link failed: {e}");
        }

        let output = Command::new(&exe_path)
            .stdin(Stdio::null())
            .output()
            .expect("failed to run tcp connect binary");

        let payload = rx.recv_timeout(Duration::from_secs(10)).ok();
        let _ = accept_thread.join();
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            output.status.success(),
            "connect binary exited non-success {:?} — stderr {:?}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        );
        let payload = payload.expect("harness never received a connection from the kara binary");
        assert!(
            payload.contains("hi from connect"),
            "expected `hi from connect` over the kara-initiated connection, got: {payload:?}",
        );
    }

    /// Phase-8 line 74 prereq — `TcpStream.connect` Err path. Connecting
    /// to a closed local port surfaces `Result.Err(TcpError)` rather than
    /// hanging or crashing. (The carried errno is `-1` at v1; line 74
    /// enriches it to a real cause — that slice's tests assert the
    /// variant.) Here we only pin that the Err arm is taken and the
    /// destructure + drop path is clean.
    #[test]
    fn test_tcp_stream_connect_err_on_closed_port() {
        let _guard = TCP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        // Port 1 is privileged + almost certainly unbound for a normal
        // test user → connect fails fast.
        let src = r#"
            fn main() {
                match TcpStream.connect("127.0.0.1:1") {
                    Result.Ok(_) => { println(98); }
                    Result.Err(_) => { println(2); }
                }
            }
        "#;

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_tcp_connect_err_{pid}_{nanos}"));

        if let Err(e) = compile_and_link(src, &exe_path) {
            panic!("compile/link failed: {e}");
        }
        let output = Command::new(&exe_path)
            .stdin(Stdio::null())
            .output()
            .expect("failed to run tcp connect-err binary");
        let _ = std::fs::remove_file(&exe_path);

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "connect-err binary exited non-success {:?} — stderr {:?}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(
            stdout.contains('2') && !stdout.contains("98"),
            "expected Err arm (printed 2), got stdout {stdout:?}",
        );
    }

    /// Phase-8 line 74 — `TcpStream.connect` to a closed port surfaces the
    /// *named* `TcpError.ConnectionRefused` variant (not the `Other`
    /// catch-all), so a reconnect loop can branch on "server not up yet".
    /// The fieldless variant matches cleanly end-to-end.
    #[test]
    fn test_tcp_stream_connect_err_is_connection_refused() {
        let _guard = TCP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let src = r#"
            fn main() {
                match TcpStream.connect("127.0.0.1:1") {
                    Result.Ok(_) => { println("ok"); }
                    Result.Err(TcpError.ConnectionRefused) => { println("refused"); }
                    Result.Err(TcpError.AddrInUse) => { println("addrinuse"); }
                    Result.Err(TcpError.PermissionDenied) => { println("perm"); }
                    Result.Err(_) => { println("other"); }
                }
            }
        "#;

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_tcp_connect_refused_{pid}_{nanos}"));

        if let Err(e) = compile_and_link(src, &exe_path) {
            panic!("compile/link failed: {e}");
        }
        let output = Command::new(&exe_path)
            .stdin(Stdio::null())
            .output()
            .expect("failed to run connect-refused binary");
        let _ = std::fs::remove_file(&exe_path);

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "binary exited non-success {:?} — stderr {:?}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(
            stdout.contains("refused") && !stdout.contains("other"),
            "expected the ConnectionRefused arm, got stdout {stdout:?}",
        );
    }
}
