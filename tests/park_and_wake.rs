//! Phase 6 line 17 — programmatic park-and-wake E2E test.
//!
//! Compiles a 5-line Kāra program that (1) binds a TCP listener via
//! the runtime test-helper FFI `karac_runtime_test_bind_and_print_port`,
//! (2) calls `karac_park_on_fd(fd, 0)` to register the fd and block
//! until readability, (3) prints `WOKEN` and exits. The Rust harness
//! reads `BOUND_PORT=<n>` from the binary's stdout, connects to that
//! port from a worker thread to trigger fd readability, and asserts
//! the binary exits successfully within a timeout.
//!
//! **What this de-risks.** Slice 6 (2026-05-24) shipped the codegen
//! lowering for `karac_park_on_fd` and pinned the IR shape with
//! IR-grep tests — but until this test ran, the **bridge** between
//! codegen and the runtime FFI had never been exercised at runtime:
//! the calling convention for `karac_runtime_event_loop_register_fd`
//! (passing `&parked_task` as `*mut c_void`), the `KaracParkedTask`
//! layout claim under opaque pointers, the state_1 blocking
//! `take_wakeups` semantics, and the round-trip of the parked pointer
//! through the event loop's wakeup queue. This test exercises all
//! four under real I/O.
//!
//! **Subprocess + port-from-stdout pattern.** Same shape as
//! `tests/http_server.rs::test_http_server_serves_hardcoded_handler`:
//! the binary writes `BOUND_PORT=<n>\n` to stdout from inside the
//! test-helper FFI immediately before returning the raw fd; the
//! harness reads stdout until it observes that line, parses the port,
//! and only then attempts the connect. Bound-port readback semantics
//! and subprocess exit/timeout handling were debugged once against
//! this minimal source; future stdlib-shaped tests (`TcpListener` /
//! `TcpStream`) inherit the harness shape.
//!
//! **Test-helper FFI gate.** `karac_runtime_test_bind_and_print_port`
//! is gated behind the `test-helpers` cargo feature on the runtime
//! crate — `runtime_path()` below builds with `--features
//! test-helpers` so the symbol is in `libkarac_runtime.a`. Production
//! binaries never see this symbol.

#[cfg(all(unix, feature = "llvm"))]
mod park_and_wake_tests {
    use std::io::{BufRead, BufReader};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::{Mutex, Once};
    use std::time::{Duration, Instant};

    // Test isolation: like `tests/http_server.rs`, this test calls
    // `std::env::set_var("KARAC_RUNTIME", ...)` before linking the
    // generated binary, and env-var mutation is not thread-safe on
    // edition 2024+. Only one test in this binary at present, but
    // we follow the established pattern so future additions don't
    // race.
    static PARK_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    static RUNTIME_BUILT: Once = Once::new();
    static mut RUNTIME_PATH: Option<PathBuf> = None;

    /// Build the runtime static library (with `test-helpers` so the
    /// test FFI symbol is exported) once per test process and return
    /// its path. Returns None if the build fails — caller soft-skips.
    /// Mirrors `tests/http_server.rs::runtime_path` plus the
    /// `--features test-helpers` flag.
    #[allow(static_mut_refs)]
    fn runtime_path() -> Option<PathBuf> {
        RUNTIME_BUILT.call_once(|| {
            let output = Command::new("cargo")
                .args([
                    "build",
                    "-p",
                    "karac-runtime",
                    "--release",
                    "--features",
                    "test-helpers",
                ])
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

    /// Compile a Kāra source string to an executable at `exe_path`.
    /// Lifted from `tests/http_server.rs::compile_and_link`.
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
        let obj = format!("/tmp/karac_park_e2e_{pid}_{nanos}.o");
        compile_to_object_with_options(&parsed.program, &obj, None, None, None, None)
            .map_err(|e| format!("codegen failed: {e}"))?;
        link_executable(&obj, exe_path.to_str().unwrap())
            .map_err(|e| format!("link failed: {e}"))?;
        let _ = std::fs::remove_file(&obj);
        Ok(())
    }

    /// Read stdout line-by-line until we see `BOUND_PORT=<n>`, return
    /// the port. Returns None on timeout. The reader keeps draining
    /// stdout for the rest of the child's lifetime so the OS pipe
    /// buffer doesn't fill and block the binary. Returns the
    /// JoinHandle so the caller can join it after the child exits.
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

    /// The primary deliverable. Compiles a kara source that parks on
    /// a TCP listener fd, runs it, connects to trigger readability,
    /// asserts the binary returns Ready and exits 0 within a timeout.
    #[test]
    fn test_park_and_wake_round_trip() {
        let _guard = PARK_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!(
                "skip: libkarac_runtime.a not built with --features test-helpers \
                 (run `cargo build -p karac-runtime --release --features test-helpers`)"
            );
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        // Kara source: declare the test-helper FFI in an `unsafe
        // extern "C"` block (no network effects — bind is a one-shot
        // syscall, not a network read/write), declare `karac_park_on_fd`
        // as the empty-bodied network-effect leaf primitive (codegen
        // special-cases this name), call bind → park → exit.
        //
        // The `println("WOKEN")` is observational only: if the test
        // hangs, it never prints; if Ready returns but the test still
        // somehow fails, the stdout pre-exit gives a debug breadcrumb.
        let src = r#"
            effect resource Network;

            unsafe extern "C" {
                fn karac_runtime_test_bind_and_print_port() -> i32;
            }

            pub fn karac_park_on_fd(fd: i32, direction: u8) with sends(Network) receives(Network) {}

            fn main() {
                let fd = karac_runtime_test_bind_and_print_port();
                let dir: u8 = 0;
                karac_park_on_fd(fd, dir);
                println("WOKEN");
            }
        "#;

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_park_e2e_{pid}_{nanos}"));

        if let Err(e) = compile_and_link(src, &exe_path) {
            panic!("compile/link failed: {e}");
        }

        let mut child = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn park-and-wake binary");

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

        // Trigger fd readability. The binary is now blocked in
        // `take_wakeups` inside `karac_park_on_fd`'s state_1 poll
        // body; a TCP connection to the listener fd makes it
        // readable, which the background event-loop poller picks up
        // and delivers via the wakeup queue.
        //
        // Small retry loop because the bound-port print happens
        // *before* the parking primitive registers the fd with the
        // event loop (the kara source runs sequentially: bind →
        // park). A connect that races between the print and the
        // register would still succeed at the TCP level but might
        // not deliver readability if the registration arms it edge-
        // triggered. mio's default level-triggered registration
        // means a missed-edge isn't an issue here, but the retry
        // loop is cheap insurance and matches the http_server
        // precedent for connect-side retry.
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
            panic!("could not connect to 127.0.0.1:{port} to trigger readability");
        }

        // Wait for the binary to observe the wakeup and exit. If
        // parking is correctly wired, the binary should return Ready
        // within milliseconds of the connect; the 10s ceiling is for
        // CI-machine slowness, not for genuine wakeup latency.
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
                             the wakeup is not being delivered to the parked task. \
                             Likely culprits: KaracParkedTask layout mismatch, \
                             wakeup queue drain bug, or state_1 not blocking correctly."
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

        // Reap the reader thread so it doesn't leak; the child's
        // stdout EOF will let it return on its own once the child
        // has exited.
        let _ = join.join();
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "binary exited with non-success status {exit_status:?} — \
             parking primitive returned Ready but main() failed somewhere downstream"
        );
    }

    /// Async-sched slice 2/3 regression: parking the **same fd twice**
    /// must work. The dispatcher-yield model deregisters the fd after each
    /// park completes (one-shot), so the second `karac_park_on_fd` on the
    /// same fd re-registers cleanly. Without that deregister, the second
    /// `register_fd` would hit `epoll_ctl(ADD)` on an already-registered
    /// fd → `EEXIST` → token 0 → the park would never receive a wakeup and
    /// the binary would hang. This is the exact shape the demo's accept
    /// loop needs (re-park the listener every iteration).
    ///
    /// One client connection satisfies both parks: the program never
    /// `accept`s, so the listener stays readable in the backlog. Park 1
    /// fires on the pending connection, deregisters; park 2 re-registers
    /// and fires on the still-pending connection.
    #[test]
    fn test_park_twice_same_fd_reregisters() {
        let _guard = PARK_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!(
                "skip: libkarac_runtime.a not built with --features test-helpers \
                 (run `cargo build -p karac-runtime --release --features test-helpers`)"
            );
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let src = r#"
            effect resource Network;

            unsafe extern "C" {
                fn karac_runtime_test_bind_and_print_port() -> i32;
            }

            pub fn karac_park_on_fd(fd: i32, direction: u8) with sends(Network) receives(Network) {}

            fn main() {
                let fd = karac_runtime_test_bind_and_print_port();
                let dir: u8 = 0;
                karac_park_on_fd(fd, dir);
                println("WOKEN1");
                karac_park_on_fd(fd, dir);
                println("WOKEN2");
            }
        "#;

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_park_twice_e2e_{pid}_{nanos}"));

        if let Err(e) = compile_and_link(src, &exe_path) {
            panic!("compile/link failed: {e}");
        }

        let mut child = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn park-twice binary");

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

        // A single connection, left un-accepted, makes the listener
        // readable for both parks.
        let connect_started = Instant::now();
        let mut connected = false;
        let mut _hold = None;
        for _ in 0..10 {
            match std::net::TcpStream::connect(format!("127.0.0.1:{port}")) {
                Ok(s) => {
                    // Hold the stream open so the connection stays in the
                    // listener's backlog (readable) across both parks.
                    _hold = Some(s);
                    connected = true;
                    break;
                }
                Err(_) => {
                    if connect_started.elapsed() > Duration::from_secs(2) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
        if !connected {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&exe_path);
            panic!("could not connect to 127.0.0.1:{port} to trigger readability");
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
                            "binary did not exit within 10s — the second park on the same fd \
                             did not receive a wakeup. Likely the fd was not deregistered after \
                             the first park, so re-registration hit EEXIST."
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
            "park-twice binary exited with non-success status {exit_status:?}"
        );
    }
}
