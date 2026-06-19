//! Relay bench harness (2026-06-19) — the wrk-based 3-language Layer-7
//! reverse-proxy benchmark (kara, go, node forwarding to one shared upstream).
//! Mirrors `tests/parallax_bench.rs`.
//!
//! Two tests (gated on `--features llvm`):
//!
//! * `test_kara_bench_server_smoke` — compile + run the Kāra reference impl
//!   at `examples/relay/bench/kara/server.kara` (a single-upstream passthrough
//!   proxy), pointed at a stub upstream this test runs, drive ONE request
//!   end-to-end through the proxy, and assert the proxied body is the stub's
//!   constant payload. Compiles through the coroutine path
//!   (`compile_to_object_with_coro`) so the smoke test exercises the SAME
//!   network-async lowering the CLI bench binary uses — the path the
//!   multi-capture-spawn regression (`tests/coro_e2e.rs::
//!   coroutine_multi_capture_string_*`) lives on.
//!
//! * `test_bench_script_dry_run` — invoke
//!   `examples/relay/bench/bench.sh --dry-run`, assert exit 0 + stdout names
//!   all three impls (kara, go, node). Pins script syntactic correctness
//!   without paying the bench cost in CI.
//!
//! The Go/Node impls and the shared upstream deliberately have no unit tests
//! — they're reference implementations, not karac code; their correctness is
//! validated by `bench.sh` returning numbers.
//!
//! Per the F3 design lock: no throughput-number assertions. The numbers are
//! the artifact, not a regression gate. CI doesn't run `bench.sh`.

#[cfg(feature = "llvm")]
mod relay_bench_tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::Once;
    use std::time::{Duration, Instant};

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    static RUNTIME_BUILT: Once = Once::new();
    static mut RUNTIME_PATH: Option<PathBuf> = None;

    /// Build the runtime static library once per test process and return its
    /// path; soft-skip on failure. Mirrors `tests/parallax_bench.rs`.
    #[allow(static_mut_refs)]
    fn runtime_path() -> Option<PathBuf> {
        RUNTIME_BUILT.call_once(|| {
            let output = Command::new("cargo")
                .args([
                    "rustc",
                    "-p",
                    "karac-runtime",
                    "--release",
                    "--crate-type",
                    "staticlib",
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

    /// Compile a Kāra source string to an executable through the coroutine
    /// network-async path — the same lowering the CLI `karac build` bench
    /// binary uses. Mirrors `tests/coro_e2e.rs::compile_link_coro` (minus the
    /// sanitizer arg).
    fn compile_and_link_coro(src: &str, exe_path: &Path) -> Result<(), String> {
        use karac::cli::{
            build_call_effect_subs_table, build_callee_network_yield_effect_table,
            build_callee_purely_polymorphic_effects_set, build_state_struct_layouts,
            build_yield_points_table,
        };
        use karac::codegen::{compile_to_object_with_coro, link_executable};

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
        let method_types = typed.method_callee_types.clone();
        let call_type_subs = typed.call_type_subs.clone();
        let pattern_binding_types = typed.pattern_binding_types.clone();
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck_with_typecheck_data(
            &parsed.program,
            karac::effectchecker::PublicEffectsPolicy::default(),
            karac::manifest::CompileProfile::Default,
            method_types.clone(),
            call_type_subs,
        );
        parsed.program.callee_network_yield_effect =
            build_callee_network_yield_effect_table(&effects);
        parsed.program.yield_points = build_yield_points_table(
            &parsed.program,
            &parsed.program.callee_network_yield_effect,
            &method_types,
        );
        parsed.program.state_struct_layouts = build_state_struct_layouts(
            &parsed.program,
            &parsed.program.callee_network_yield_effect,
            &method_types,
            &pattern_binding_types,
        );
        parsed.program.call_effect_subs = build_call_effect_subs_table(&effects);
        parsed.program.callee_purely_polymorphic_effects =
            build_callee_purely_polymorphic_effects_set(&effects);

        let ownership = karac::ownershipcheck(&parsed.program, &typed);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let obj = format!("/tmp/karac_relay_smoke_{pid}_{nanos}.o");
        compile_to_object_with_coro(&parsed.program, &obj, Some(&ownership), None)
            .map_err(|e| format!("codegen failed: {e}"))?;
        link_executable(&obj, exe_path.to_str().unwrap())
            .map_err(|e| format!("link failed: {e}"))?;
        let _ = std::fs::remove_file(&obj);
        Ok(())
    }

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

    /// A minimal stub upstream: bind an ephemeral port, accept one connection,
    /// read the proxied request, and reply with a fixed HTTP response whose
    /// body is `UPSTREAM_BODY`. Runs on its own thread. Returns the bound port.
    const UPSTREAM_BODY: &str = "RELAY-OK";

    fn spawn_stub_upstream() -> (u16, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("stub upstream bind");
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            // Service a few connections so a retried smoke request still lands.
            listener
                .set_nonblocking(false)
                .expect("stub upstream blocking");
            let deadline = Instant::now() + Duration::from_secs(20);
            for conn in listener.incoming() {
                if Instant::now() > deadline {
                    break;
                }
                let mut stream = match conn {
                    Ok(s) => s,
                    Err(_) => break,
                };
                stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                // Read the (proxied) request; we don't parse it — any bytes
                // mean the proxy forwarded.
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nConnection: close\r\n\r\n{}",
                    UPSTREAM_BODY.len(),
                    UPSTREAM_BODY
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });
        (port, handle)
    }

    /// Send `GET /` through the proxy on `port` and return the response body.
    fn http_get_body(port: u16) -> Result<String, String> {
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .map_err(|e| format!("connect failed: {e}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(|e| format!("set_read_timeout failed: {e}"))?;
        let req = "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
        stream
            .write_all(req.as_bytes())
            .map_err(|e| format!("write failed: {e}"))?;
        let mut buf = Vec::new();
        stream
            .read_to_end(&mut buf)
            .map_err(|e| format!("read failed: {e}"))?;
        let text = String::from_utf8_lossy(&buf).into_owned();
        let body = text
            .split("\r\n\r\n")
            .nth(1)
            .map(|s| s.to_string())
            .unwrap_or_default();
        Ok(body)
    }

    /// Compile + run the Kāra bench passthrough proxy, point it at a stub
    /// upstream via `RELAY_UPSTREAM`, drive one `GET /` through it, and assert
    /// the proxied body matches the upstream's constant payload.
    ///
    /// This is the dogfooding gate: building this `server.kara` through the
    /// coroutine path surfaced the multi-capture-spawn use-after-free (the
    /// `handle(client, upstream_addr)` closure captured the moved `TcpStream`
    /// alongside a `String`; the spawn wrapper freed the `String`'s buffer
    /// while the coroutine was still parked). Fixed in
    /// `src/codegen/task_group.rs`; regression-pinned by
    /// `tests/coro_e2e.rs::coroutine_multi_capture_string_*`. If this smoke
    /// ever fails with an empty body, that's the same bug class resurfacing.
    #[test]
    fn test_kara_bench_server_smoke() {
        let Some(rt) = runtime_path() else {
            eprintln!(
                "skip: libkarac_runtime.a not built \
                 (run `cargo rustc -p karac-runtime --release --crate-type staticlib`)"
            );
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let src_path = workspace_root().join("examples/relay/bench/kara/server.kara");
        let src = std::fs::read_to_string(&src_path)
            .unwrap_or_else(|e| panic!("missing fixture {}: {e}", src_path.display()));

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_relay_smoke_{pid}_{nanos}"));

        if let Err(e) = compile_and_link_coro(&src, &exe_path) {
            panic!("compile/link failed: {e}");
        }

        // Stand up the stub upstream and point the proxy at it.
        let (up_port, _up_join) = spawn_stub_upstream();

        let mut child = Command::new(&exe_path)
            .env("RELAY_UPSTREAM", format!("127.0.0.1:{up_port}"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn relay bench proxy binary");

        let stdout = child.stdout.take().expect("child stdout missing");
        let (port_opt, _join) = await_bound_port(stdout, Duration::from_secs(15));

        let port = match port_opt {
            Some(p) => p,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&exe_path);
                panic!("proxy did not emit BOUND_PORT line within timeout");
            }
        };
        assert!(port > 0, "BOUND_PORT must be a non-zero ephemeral port");

        let started = Instant::now();
        let mut last_err: Option<String> = None;
        let mut body: Option<String> = None;
        for _ in 0..10 {
            match http_get_body(port) {
                Ok(b) if !b.is_empty() => {
                    body = Some(b);
                    break;
                }
                Ok(_) => {
                    last_err = Some("empty proxied body".to_string());
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    last_err = Some(e);
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            if started.elapsed() > Duration::from_secs(15) {
                break;
            }
        }

        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&exe_path);

        let body = match body {
            Some(b) => b,
            None => panic!(
                "GET / through the relay proxy never returned a body; \
                 last error: {last_err:?}"
            ),
        };
        assert!(
            body.contains(UPSTREAM_BODY),
            "proxied body should contain the upstream payload `{UPSTREAM_BODY}`; got: {body:?}"
        );
    }

    /// `bench.sh --dry-run` exits 0 and lists the three impl names on stdout.
    /// Doesn't actually start any server or invoke `wrk`.
    #[test]
    fn test_bench_script_dry_run() {
        let script = workspace_root().join("examples/relay/bench/bench.sh");
        if !script.exists() {
            panic!("missing fixture: {}", script.display());
        }
        let output = Command::new("sh")
            .arg(&script)
            .arg("--dry-run")
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn bench.sh: {e}"));
        assert!(
            output.status.success(),
            "bench.sh --dry-run exited non-zero: status={:?}, stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        for impl_name in &["kara", "go", "node"] {
            assert!(
                stdout.contains(impl_name),
                "bench.sh --dry-run stdout should mention `{impl_name}`; got:\n{stdout}"
            );
        }
    }
}
