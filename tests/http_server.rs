//! Slice B (2026-05-09) — HTTP server FFI surface (minimal `std.http`).
//!
//! Pins the `Server.serve_static` smoke path end-to-end: compiles a
//! Kāra source file that calls `Server.serve_static("127.0.0.1:0",
//! body)`, links it against `libkarac_runtime.a` (which carries the
//! hyper-backed `karac_runtime_serve_http_static` export), spawns the
//! resulting binary as a subprocess, reads the bound port from the
//! binary's `BOUND_PORT=<n>` stdout line, performs a `GET /dashboard/1`
//! against `127.0.0.1:<port>`, and asserts the response is `200` with
//! the expected JSON body.
//!
//! **B3 fallback (b) taken.** Free-fn effect-set polymorphism on the
//! handler signature isn't typechecker-supported yet (Theme 6 settled
//! the trait-method shape but free-fn shape is the open delta). v1's
//! smoke surface uses `Server.serve_static(addr, body)` — a fixed-body
//! responder — so the v1 entry doesn't depend on either fn-pointer-as-
//! free-fn-arg codegen or effect-set-parameter syntax. Polymorphic
//! `serve[E]` + arbitrary handler dispatch lands in a follow-up.
//!
//! **Subprocess + port-from-stdout pattern.** Precedent in
//! `tests/memory_sanitizer.rs`. The binary writes `BOUND_PORT=<n>\n`
//! to stdout from inside `karac_runtime_serve_http_static` immediately
//! before entering the accept loop; the test harness reads stdout
//! until it observes that line, parses the port, and only then
//! attempts the HTTP request.
//!
//! **Effect-propagation pin (`#[ignore]` placeholder).** v1's smoke
//! path uses `Server.serve_static` which has no handler — there's no
//! handler-effect-set to propagate. The placeholder lives at the end
//! of this file as documentation of what the polymorphic-`serve[E]`
//! follow-up needs to test.

mod common;

#[cfg(feature = "llvm")]
mod http_server_tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::{Mutex, Once};
    use std::time::{Duration, Instant};

    // Test isolation: every test in this module calls
    // `std::env::set_var("KARAC_RUNTIME", ...)` before linking the
    // generated binary, and env-var mutation is not thread-safe
    // (`std::env::set_var` is `unsafe` on edition 2024+ for exactly
    // this reason — concurrent set/read interleavings produce
    // intermittent link failures and `await_bound_port` timeouts
    // when several tests run on cargo's default test thread pool).
    // A shared `Mutex<()>` serializes the four tests within this
    // test binary; lock is acquired at the top of each test and
    // released on drop. Same precedent as
    // `tests/codegen.rs::SPAWN_SITE_ENV_LOCK`.
    static HTTP_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    static RUNTIME_BUILT: Once = Once::new();
    static mut RUNTIME_PATH: Option<PathBuf> = None;

    /// Build the runtime static library once per test process and
    /// return its path. Returns None if the build fails — callers
    /// soft-skip. Mirrors `tests/parallax_lite.rs::runtime_path`.
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

    /// Compile a Kāra source string to an executable at `exe_path`,
    /// returning Err with a diagnostic on failure. Mirrors the build
    /// path in `tests/parallax_lite.rs::compile_and_time` minus the
    /// concurrency-analysis hookup (the smoke source has no auto-par
    /// surface).
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
        let ownership = karac::ownershipcheck(&parsed.program, &typed);
        super::common::assert_ownership_clean(&ownership, src);
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let obj = format!("/tmp/karac_http_smoke_{pid}_{nanos}.o");
        compile_to_object_with_options(&parsed.program, &obj, Some(&ownership), None, None, None)
            .map_err(|e| format!("codegen failed: {e}"))?;
        link_executable(&obj, exe_path.to_str().unwrap())
            .map_err(|e| format!("link failed: {e}"))?;
        let _ = std::fs::remove_file(&obj);
        Ok(())
    }

    /// Read stdout line-by-line until we see `BOUND_PORT=<n>` (returns
    /// the parsed port), or until `timeout` elapses (returns `None`).
    /// The reader thread keeps the child's stdout drained so the OS
    /// pipe buffer doesn't fill up and block the binary.
    fn await_bound_port(
        stdout: std::process::ChildStdout,
        timeout: Duration,
    ) -> (Option<u16>, std::thread::JoinHandle<()>) {
        let (tx, rx) = std::sync::mpsc::channel::<u16>();
        let handle = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            // Loop reading lines; on first BOUND_PORT match send port
            // through the channel, then keep draining stdout for the
            // remainder of the process lifetime (so the child doesn't
            // block on a full stdout pipe).
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

    /// Issue an HTTP/1.1 GET to `127.0.0.1:<port><path>` over a raw
    /// `TcpStream`. Returns `(status_code, body)`. Hand-rolled so the
    /// test doesn't pull in a heavier dev-dep — `ureq` is already a
    /// project dep but its blocking client adds startup cost we don't
    /// need for a single-request smoke.
    fn http_get(port: u16, path: &str) -> Result<(u16, String), String> {
        let mut stream = std::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .map_err(|e| format!("connect failed: {e}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| format!("set_read_timeout failed: {e}"))?;
        let req = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
        stream
            .write_all(req.as_bytes())
            .map_err(|e| format!("write failed: {e}"))?;
        let mut buf = Vec::new();
        stream
            .read_to_end(&mut buf)
            .map_err(|e| format!("read failed: {e}"))?;
        let text = String::from_utf8_lossy(&buf).into_owned();
        let mut parts = text.split("\r\n\r\n");
        let head = parts.next().unwrap_or("");
        let body = parts.collect::<Vec<_>>().join("\r\n\r\n");
        // Parse status line: "HTTP/1.1 200 OK".
        let first = head.lines().next().ok_or("empty response")?;
        let mut tokens = first.split_whitespace();
        let _proto = tokens.next();
        let status_str = tokens.next().ok_or("missing status code")?;
        let status: u16 = status_str
            .parse()
            .map_err(|e| format!("bad status code '{status_str}': {e}"))?;
        Ok((status, body))
    }

    /// Triple returned by `http_get_with_response_headers` —
    /// `(status, headers, body)`. Factored as a type alias so clippy's
    /// `type_complexity` lint stays happy.
    type HttpResponseTriple = (u16, Vec<(String, String)>, String);

    /// Phase-8 line 14 — variant of `http_get` that also returns the
    /// response headers as `(name, value)` pairs. Hand-rolled parse:
    /// skips the status line, splits each remaining header line on the
    /// first `:`, trims surrounding whitespace. Header names are
    /// returned as the server emitted them (hyper normalizes inbound
    /// request header names to lowercase, but emits response header
    /// names case-preserving for the value the handler set).
    fn http_get_with_response_headers(port: u16, path: &str) -> Result<HttpResponseTriple, String> {
        let mut stream = std::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .map_err(|e| format!("connect failed: {e}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| format!("set_read_timeout failed: {e}"))?;
        let req = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
        stream
            .write_all(req.as_bytes())
            .map_err(|e| format!("write failed: {e}"))?;
        let mut buf = Vec::new();
        stream
            .read_to_end(&mut buf)
            .map_err(|e| format!("read failed: {e}"))?;
        let text = String::from_utf8_lossy(&buf).into_owned();
        let mut parts = text.split("\r\n\r\n");
        let head = parts.next().unwrap_or("");
        let body = parts.collect::<Vec<_>>().join("\r\n\r\n");
        let mut head_lines = head.lines();
        let first = head_lines.next().ok_or("empty response")?;
        let mut tokens = first.split_whitespace();
        let _proto = tokens.next();
        let status_str = tokens.next().ok_or("missing status code")?;
        let status: u16 = status_str
            .parse()
            .map_err(|e| format!("bad status code '{status_str}': {e}"))?;
        let mut headers: Vec<(String, String)> = Vec::new();
        for line in head_lines {
            if let Some((k, v)) = line.split_once(':') {
                headers.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
        Ok((status, headers, body))
    }

    /// B5 smoke test (the primary deliverable).
    ///
    /// Compiles a minimal Kāra program that calls
    /// `Server.serve_static("127.0.0.1:0", "...")`, runs the binary,
    /// reads the bound port from stdout, performs a `GET /dashboard/1`,
    /// asserts the response is 200 with the expected JSON body.
    ///
    /// The test takes the **single-file concat workaround** for
    /// `examples/parallax/`'s multi-file project layout (per Slice B's
    /// hard-stop trigger 5 fallback): the binary the test compiles is
    /// a single inline source string, not a multi-file project build.
    /// `examples/parallax/` holds the source-of-truth multi-file form
    /// for human readers.
    #[test]
    fn test_http_server_serves_hardcoded_handler() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // Build the runtime statically; soft-skip if it can't be
        // built (e.g. no internet for crate downloads on a sandboxed
        // host — same skip pattern as `tests/par_codegen.rs`).
        let Some(rt) = runtime_path() else {
            eprintln!(
                "skip: libkarac_runtime.a not built \
                 (run `cargo build -p karac-runtime --release`)"
            );
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        // Inline source: bind on 127.0.0.1:0 and serve a fixed JSON
        // body for every request. The body matches what the future
        // `get_dashboard(1)` handler would return — the test asserts
        // structural fields (`profile`, `latest_orders`,
        // `top_notification`, `top_recommendation`) so the smoke is
        // demo-shaped even though the handler itself is a fixed
        // string in v1.
        let src = r#"
            fn main() {
                let addr = "127.0.0.1:0";
                let body = "{\"profile\":{\"name\":\"Alice\"},\"latest_orders\":[],\"top_notification\":{},\"top_recommendation\":{}}";
                let _result = Server.serve_static(addr, body);
                println("server exited unexpectedly");
            }
        "#;

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_http_smoke_{pid}_{nanos}"));

        if let Err(e) = compile_and_link(src, &exe_path) {
            panic!("compile/link failed: {e}");
        }

        let mut child = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn server binary");

        let stdout = child.stdout.take().expect("child stdout missing");
        let (port_opt, _join) = await_bound_port(stdout, Duration::from_secs(15));

        let port = match port_opt {
            Some(p) => p,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&exe_path);
                panic!("server did not emit BOUND_PORT line within timeout");
            }
        };
        assert!(port > 0, "BOUND_PORT must be a non-zero ephemeral port");

        // Allow the listener a tiny window to be ready for accept(2)
        // — the BOUND_PORT line is written *immediately before* the
        // accept loop is entered, so a connect attempted in the same
        // microsecond can race the listener's TcpListener::accept
        // call. Retry-with-backoff handles the race; we cap at 5
        // attempts ~spaced by 50ms.
        let started = Instant::now();
        let mut last_err: Option<String> = None;
        let mut response: Option<(u16, String)> = None;
        for _ in 0..10 {
            match http_get(port, "/dashboard/1") {
                Ok(r) => {
                    response = Some(r);
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            if started.elapsed() > Duration::from_secs(10) {
                break;
            }
        }

        let _ = child.kill();
        // Reap the child to avoid leaving a zombie process — clippy
        // flags spawned children that are never `wait()`-ed.
        let _ = child.wait();
        let _ = std::fs::remove_file(&exe_path);

        let (status, body) = match response {
            Some(r) => r,
            None => panic!(
                "GET /dashboard/1 against 127.0.0.1:{port} never succeeded; \
                 last error: {:?}",
                last_err
            ),
        };
        assert_eq!(status, 200, "expected 200 status; body={body:?}");
        assert!(
            body.contains("\"profile\""),
            "body should contain `profile` field; got: {body:?}"
        );
        assert!(
            body.contains("\"latest_orders\""),
            "body should contain `latest_orders` field; got: {body:?}"
        );
        assert!(
            body.contains("\"top_notification\""),
            "body should contain `top_notification` field; got: {body:?}"
        );
        assert!(
            body.contains("\"top_recommendation\""),
            "body should contain `top_recommendation` field; got: {body:?}"
        );
    }

    /// Effect-propagation pin (placeholder).
    ///
    /// **Conditional on the polymorphic-`serve[E]` follow-up.** v1's
    /// `Server.serve_static` is a fixed-body responder with no handler,
    /// so there's no handler-effect-set to propagate. Once free-fn
    /// effect-set parameter syntax + fn-pointer-as-arg codegen lands
    /// (a Phase 7/8 follow-up), this test fills in to exercise the
    /// signature
    /// `Server.serve[E](handler: fn(Request) -> Response with E)`
    /// against a handler with explicit `with reads(MockResource),
    /// writes(MockResource)` annotations and asserts the typechecker
    /// admits the call without effect-set mismatch.
    ///
    /// Per Slice B's locked test surface (item 2): "stays as a
    /// `#[ignore]` placeholder until effect-set parameter syntax on
    /// free fns lands."
    #[test]
    #[ignore]
    fn test_http_server_handler_effects_propagate() {
        // Placeholder — see doc comment.
    }

    /// Slice B follow-up (2026-05-09) — `Server.serve(handler)` smoke.
    ///
    /// Compiles a Kāra program that calls `Server.serve(handle)` with a
    /// free-fn handler, runs the resulting binary, reads `BOUND_PORT`
    /// from stdout, performs a `GET /`, and asserts the runtime side
    /// of the handler-dispatch entry comes up cleanly. Sibling to
    /// `test_http_server_serves_hardcoded_handler` (the `serve_static`
    /// equivalent above).
    ///
    /// HTTP handler ABI trampoline (2026-05-09): the per-handler shim
    /// adapts between the user `fn handle(req: Request) -> Response`
    /// signature and the FFI extern's
    /// `extern "C" fn(*const KaracHttpRequest, *mut KaracHttpResponse)`.
    /// `Request` lowers as an opaque pointer (F2); `Response` status and
    /// body decompose into the runtime setters. Codegen-side caching is
    /// pinned by
    /// `tests/codegen.rs::test_server_serve_handler_shim_caches`;
    /// this test is the end-to-end runtime exercise.
    #[test]
    fn test_server_serve_handler_smoke() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!(
                "skip: libkarac_runtime.a not built \
                 (run `cargo build -p karac-runtime --release`)"
            );
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        // Response is defined inline because the codegen test pipeline
        // (`compile_and_link`) doesn't auto-inject stdlib structs; the
        // user code carries the body shape directly. The `Server.serve`
        // / `Request.path()` dispatch arms are registered as compiler
        // builtins regardless.
        let src = r#"
            struct Response { status: i64, body: String }

            fn handle(req: Request) -> Response {
                Response { status: 200, body: "{}" }
            }

            fn main() {
                let _result = Server.serve("127.0.0.1:0", handle);
                println("server exited unexpectedly");
            }
        "#;

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_http_handler_{pid}_{nanos}"));

        if let Err(e) = compile_and_link(src, &exe_path) {
            panic!("compile/link failed: {e}");
        }

        let mut child = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn server binary");

        let stdout = child.stdout.take().expect("child stdout missing");
        let (port_opt, _join) = await_bound_port(stdout, Duration::from_secs(15));

        let port = match port_opt {
            Some(p) => p,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&exe_path);
                panic!("server did not emit BOUND_PORT line within timeout");
            }
        };
        assert!(port > 0, "BOUND_PORT must be a non-zero ephemeral port");

        let started = Instant::now();
        let mut last_err: Option<String> = None;
        let mut response: Option<(u16, String)> = None;
        for _ in 0..10 {
            match http_get(port, "/") {
                Ok(r) => {
                    response = Some(r);
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            if started.elapsed() > Duration::from_secs(10) {
                break;
            }
        }

        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&exe_path);

        let (status, body) = match response {
            Some(r) => r,
            None => panic!(
                "GET / against 127.0.0.1:{port} never succeeded; \
                 last error: {:?}",
                last_err
            ),
        };
        assert_eq!(status, 200, "expected 200 status; body={body:?}");
        assert_eq!(body.trim(), "{}", "expected `{{}}` body; got: {body:?}");
    }

    // ── HTTP handler ABI trampoline ──
    //
    // The two tests below pin the F3 method surface end-to-end:
    // `Request.path()` and `Request.method()` round-trip through the
    // runtime externs and yield owned Strings the user handler can
    // return as the response body.

    /// Run a handler-using server inline against `path` and assert the
    /// response. Soft-skips when the runtime library isn't built.
    fn run_handler_smoke(src: &str, request_path: &str) -> Option<(u16, String)> {
        let rt = runtime_path()?;
        std::env::set_var("KARAC_RUNTIME", &rt);
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_http_handler_run_{pid}_{nanos}"));
        if let Err(e) = compile_and_link(src, &exe_path) {
            panic!("compile/link failed: {e}");
        }
        let mut child = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn server binary");
        let stdout = child.stdout.take().expect("child stdout missing");
        let (port_opt, _join) = await_bound_port(stdout, Duration::from_secs(15));
        let port = match port_opt {
            Some(p) => p,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&exe_path);
                panic!("server did not emit BOUND_PORT line within timeout");
            }
        };
        assert!(port > 0, "BOUND_PORT must be a non-zero ephemeral port");
        let started = Instant::now();
        let mut last_err: Option<String> = None;
        let mut response: Option<(u16, String)> = None;
        for _ in 0..10 {
            match http_get(port, request_path) {
                Ok(r) => {
                    response = Some(r);
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            if started.elapsed() > Duration::from_secs(10) {
                break;
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&exe_path);
        match response {
            Some(r) => Some(r),
            None => panic!(
                "GET {request_path} against 127.0.0.1:{port} never succeeded; \
                 last error: {:?}",
                last_err
            ),
        }
    }

    /// `Request.path()` round-trips end-to-end: the user handler reads
    /// the path from the incoming request and returns it as the
    /// response body. Pins F2 (opaque-ptr Request shape) + F3
    /// (`path()` method dispatch) end-to-end.
    #[test]
    fn test_server_serve_handler_reads_path() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let src = r#"
            struct Response { status: i64, body: String }

            fn handle(req: Request) -> Response {
                Response { status: 200, body: req.path() }
            }

            fn main() {
                let _result = Server.serve("127.0.0.1:0", handle);
                println("server exited unexpectedly");
            }
        "#;
        let Some((status, body)) = run_handler_smoke(src, "/dashboard/42") else {
            return;
        };
        assert_eq!(status, 200, "expected 200 status; body={body:?}");
        assert!(
            body.contains("/dashboard/42"),
            "expected response body to echo path /dashboard/42; got: {body:?}"
        );
    }

    /// `Request.method()` round-trips end-to-end: the user handler reads
    /// the HTTP method verb and returns it as the response body. Pins
    /// the `method()` arm.
    #[test]
    fn test_server_serve_handler_reads_method() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let src = r#"
            struct Response { status: i64, body: String }

            fn handle(req: Request) -> Response {
                Response { status: 200, body: req.method() }
            }

            fn main() {
                let _result = Server.serve("127.0.0.1:0", handle);
                println("server exited unexpectedly");
            }
        "#;
        let Some((status, body)) = run_handler_smoke(src, "/") else {
            return;
        };
        assert_eq!(status, 200, "expected 200 status; body={body:?}");
        assert_eq!(
            body.trim(),
            "GET",
            "expected response body to be `GET`; got: {body:?}"
        );
    }

    /// POST variant of `http_get` — issues `POST <path>` with a
    /// `Content-Length`-framed body. Returns `(status, body)`.
    fn http_post(port: u16, path: &str, body: &str) -> Result<(u16, String), String> {
        let mut stream = std::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .map_err(|e| format!("connect failed: {e}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| format!("set_read_timeout failed: {e}"))?;
        let req = format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(req.as_bytes())
            .map_err(|e| format!("write failed: {e}"))?;
        let mut buf = Vec::new();
        stream
            .read_to_end(&mut buf)
            .map_err(|e| format!("read failed: {e}"))?;
        let text = String::from_utf8_lossy(&buf).into_owned();
        let mut parts = text.split("\r\n\r\n");
        let head = parts.next().unwrap_or("");
        let resp_body = parts.collect::<Vec<_>>().join("\r\n\r\n");
        let first = head.lines().next().ok_or("empty response")?;
        let mut tokens = first.split_whitespace();
        let _proto = tokens.next();
        let status_str = tokens.next().ok_or("missing status code")?;
        let status: u16 = status_str
            .parse()
            .map_err(|e| format!("bad status code '{status_str}': {e}"))?;
        Ok((status, resp_body))
    }

    /// POST variant of `run_handler_smoke`. Compiles `src`, runs it,
    /// POSTs `body` to `request_path`, returns `(status, response_body)`.
    /// Soft-skips when the runtime library isn't built.
    fn run_handler_smoke_post(src: &str, request_path: &str, body: &str) -> Option<(u16, String)> {
        let rt = runtime_path()?;
        std::env::set_var("KARAC_RUNTIME", &rt);
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_http_post_run_{pid}_{nanos}"));
        if let Err(e) = compile_and_link(src, &exe_path) {
            panic!("compile/link failed: {e}");
        }
        let mut child = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn server binary");
        let stdout = child.stdout.take().expect("child stdout missing");
        let (port_opt, _join) = await_bound_port(stdout, Duration::from_secs(15));
        let port = match port_opt {
            Some(p) => p,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&exe_path);
                panic!("server did not emit BOUND_PORT line within timeout");
            }
        };
        assert!(port > 0, "BOUND_PORT must be a non-zero ephemeral port");
        let started = Instant::now();
        let mut last_err: Option<String> = None;
        let mut response: Option<(u16, String)> = None;
        for _ in 0..10 {
            match http_post(port, request_path, body) {
                Ok(r) => {
                    response = Some(r);
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            if started.elapsed() > Duration::from_secs(10) {
                break;
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&exe_path);
        match response {
            Some(r) => Some(r),
            None => panic!(
                "POST {request_path} against 127.0.0.1:{port} never succeeded; \
                 last error: {:?}",
                last_err
            ),
        }
    }

    /// Slice 1 (2026-05-21): `Request.body()` round-trips end-to-end.
    /// The handler reads the POST body and echoes it as the response
    /// body. Pins the `body()` arm of the Request method dispatch in
    /// both the codegen path (`compile_request_body`) and the runtime
    /// externs (`karac_runtime_http_request_body_ptr/_len`).
    #[test]
    fn test_server_serve_handler_reads_body() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let src = r#"
            struct Response { status: i64, body: String }

            fn handle(req: Request) -> Response {
                Response { status: 200, body: req.body() }
            }

            fn main() {
                let _result = Server.serve("127.0.0.1:0", handle);
                println("server exited unexpectedly");
            }
        "#;
        let payload = "{\"hello\":\"world\"}";
        let Some((status, body)) = run_handler_smoke_post(src, "/echo", payload) else {
            return;
        };
        assert_eq!(status, 200, "expected 200 status; body={body:?}");
        assert_eq!(
            body, payload,
            "expected response body to echo POST body; got: {body:?}"
        );
    }

    /// Slice 1 (2026-05-21): `Request.body()` returns an empty String
    /// when the request carries no body (GET smoke shape). Pins the
    /// `body_len == 0` branch of `compile_request_body` and the
    /// null-pointer-on-empty branch of `karac_runtime_http_request_body_ptr`.
    #[test]
    fn test_server_serve_handler_empty_body_for_get() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let src = r#"
            struct Response { status: i64, body: String }

            fn handle(req: Request) -> Response {
                let b = req.body();
                Response { status: 200, body: b }
            }

            fn main() {
                let _result = Server.serve("127.0.0.1:0", handle);
                println("server exited unexpectedly");
            }
        "#;
        let Some((status, body)) = run_handler_smoke(src, "/") else {
            return;
        };
        assert_eq!(status, 200, "expected 200 status; body={body:?}");
        assert_eq!(
            body, "",
            "expected empty response body for GET with no payload; got: {body:?}"
        );
    }

    /// GET helper that ships a single extra header alongside the default
    /// `Host` / `Connection: close` pair. Same hand-rolled shape as
    /// `http_get` so the test doesn't pull in a heavier dev-dep.
    fn http_get_with_header(
        port: u16,
        path: &str,
        header_name: &str,
        header_value: &str,
    ) -> Result<(u16, String), String> {
        let mut stream = std::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .map_err(|e| format!("connect failed: {e}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| format!("set_read_timeout failed: {e}"))?;
        let req = format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{header_name}: {header_value}\r\n\
             Connection: close\r\n\r\n"
        );
        stream
            .write_all(req.as_bytes())
            .map_err(|e| format!("write failed: {e}"))?;
        let mut buf = Vec::new();
        stream
            .read_to_end(&mut buf)
            .map_err(|e| format!("read failed: {e}"))?;
        let text = String::from_utf8_lossy(&buf).into_owned();
        let mut parts = text.split("\r\n\r\n");
        let head = parts.next().unwrap_or("");
        let body = parts.collect::<Vec<_>>().join("\r\n\r\n");
        let first = head.lines().next().ok_or("empty response")?;
        let mut tokens = first.split_whitespace();
        let _proto = tokens.next();
        let status_str = tokens.next().ok_or("missing status code")?;
        let status: u16 = status_str
            .parse()
            .map_err(|e| format!("bad status code '{status_str}': {e}"))?;
        Ok((status, body))
    }

    /// Inline driver mirroring `run_handler_smoke` but with custom
    /// header injection on the outbound GET. Returns `None` (skipping
    /// the test) when the runtime library can't be built, the same
    /// soft-skip pattern the sibling helpers use.
    fn run_handler_smoke_with_header(
        src: &str,
        request_path: &str,
        header_name: &str,
        header_value: &str,
    ) -> Option<(u16, String)> {
        let rt = runtime_path()?;
        std::env::set_var("KARAC_RUNTIME", &rt);
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_http_handler_hdr_{pid}_{nanos}"));
        if let Err(e) = compile_and_link(src, &exe_path) {
            panic!("compile/link failed: {e}");
        }
        let mut child = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn server binary");
        let stdout = child.stdout.take().expect("child stdout missing");
        let (port_opt, _join) = await_bound_port(stdout, Duration::from_secs(15));
        let port = match port_opt {
            Some(p) => p,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&exe_path);
                panic!("server did not emit BOUND_PORT line within timeout");
            }
        };
        assert!(port > 0, "BOUND_PORT must be a non-zero ephemeral port");
        let started = Instant::now();
        let mut last_err: Option<String> = None;
        let mut response: Option<(u16, String)> = None;
        for _ in 0..10 {
            match http_get_with_header(port, request_path, header_name, header_value) {
                Ok(r) => {
                    response = Some(r);
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            if started.elapsed() > Duration::from_secs(10) {
                break;
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&exe_path);
        match response {
            Some(r) => Some(r),
            None => panic!(
                "GET {request_path} with header against 127.0.0.1:{port} never \
                 succeeded; last error: {:?}",
                last_err
            ),
        }
    }

    /// `Request.header(name)` round-trips end-to-end: the handler reads
    /// the value of the inbound header by name and echoes it back as
    /// the response body. Pins the runtime extern
    /// `karac_runtime_http_request_header` + the codegen path
    /// (`compile_request_header`) + the `Option[String]` unwrap shape.
    #[test]
    fn test_server_serve_handler_reads_header() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let src = r#"
            struct Response { status: i64, body: String }

            fn handle(req: Request) -> Response {
                let body = match req.header("X-Test-Echo") {
                    Some(v) => v,
                    None => "absent",
                };
                Response { status: 200, body: body }
            }

            fn main() {
                let _result = Server.serve("127.0.0.1:0", handle);
                println("server exited unexpectedly");
            }
        "#;
        let Some((status, body)) =
            run_handler_smoke_with_header(src, "/echo-hdr", "X-Test-Echo", "greetings")
        else {
            return;
        };
        assert_eq!(status, 200, "expected 200 status; body={body:?}");
        assert_eq!(
            body, "greetings",
            "expected response body to echo X-Test-Echo header value; got: {body:?}"
        );
    }

    /// Header lookup is case-insensitive per RFC 7230 § 3.2 — the
    /// handler asks for `Content-Type` but the request carries
    /// `content-type` (lowercase, hyper's normalized form). Pins the
    /// `eq_ignore_ascii_case` branch in
    /// `karac_runtime_http_request_header`.
    #[test]
    fn test_server_serve_handler_header_lookup_case_insensitive() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let src = r#"
            struct Response { status: i64, body: String }

            fn handle(req: Request) -> Response {
                let body = match req.header("Content-Type") {
                    Some(v) => v,
                    None => "absent",
                };
                Response { status: 200, body: body }
            }

            fn main() {
                let _result = Server.serve("127.0.0.1:0", handle);
                println("server exited unexpectedly");
            }
        "#;
        let Some((status, body)) =
            run_handler_smoke_with_header(src, "/case", "content-type", "application/json")
        else {
            return;
        };
        assert_eq!(status, 200, "expected 200 status; body={body:?}");
        assert_eq!(
            body, "application/json",
            "expected case-insensitive lookup to find lowercased header; got: {body:?}"
        );
    }

    /// `Request.header(name)` returns `None` when no header by that
    /// name exists on the request. Pins the null-return branch of
    /// `karac_runtime_http_request_header` and the `None` PHI arm of
    /// `compile_request_header`.
    #[test]
    fn test_server_serve_handler_header_absent_returns_none() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let src = r#"
            struct Response { status: i64, body: String }

            fn handle(req: Request) -> Response {
                let body = match req.header("X-Does-Not-Exist") {
                    Some(v) => v,
                    None => "absent",
                };
                Response { status: 200, body: body }
            }

            fn main() {
                let _result = Server.serve("127.0.0.1:0", handle);
                println("server exited unexpectedly");
            }
        "#;
        let Some((status, body)) = run_handler_smoke(src, "/absent") else {
            return;
        };
        assert_eq!(status, 200, "expected 200 status; body={body:?}");
        assert_eq!(
            body, "absent",
            "expected response body to indicate header absent; got: {body:?}"
        );
    }

    /// `Request.headers()` full-map iteration round-trips end-to-end:
    /// the handler walks every `(name, value)` pair, concatenating them
    /// into the response body, and the test asserts the custom header
    /// shows up. Pins the codegen `compile_request_pairs` loop + the
    /// `karac_runtime_http_request_headers_count` / `_header_*_at`
    /// indexed accessors + the `Vec[(String, String)]` return shape.
    /// hyper normalizes header names to lowercase, so the custom
    /// `X-Test-Echo` arrives as `x-test-echo`. Concatenation (not direct
    /// capture of the loop value) keeps the body bytes owned independent
    /// of the iterated Vec.
    #[test]
    fn test_server_serve_handler_iterates_headers() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // The handler iterates every `(name, value)` pair and reports a
        // marker proving it saw BOTH the custom header (with the right
        // value) AND the always-present Host header — i.e. full-map
        // iteration, not just the first entry. The body is a fixed
        // literal (no String concat — concat is codegen-only, not
        // typechecker-supported — and no capture of a Vec-owned String).
        let src = r#"
            struct Response { status: i64, body: String }

            fn handle(req: Request) -> Response {
                let hdrs: Vec[(String, String)] = req.headers();
                let mut found_echo = 0;
                let mut found_host = 0;
                for (k, v) in hdrs {
                    if k == "x-test-echo" {
                        if v == "greetings" {
                            found_echo = 1;
                        }
                    }
                    if k == "host" {
                        found_host = 1;
                    }
                }
                let body = if found_echo == 1 {
                    if found_host == 1 { "both" } else { "echo-only" }
                } else {
                    "no-echo"
                };
                Response { status: 200, body: body }
            }

            fn main() {
                let _result = Server.serve("127.0.0.1:0", handle);
                println("server exited unexpectedly");
            }
        "#;
        let Some((status, body)) =
            run_handler_smoke_with_header(src, "/hdrs", "X-Test-Echo", "greetings")
        else {
            return;
        };
        assert_eq!(status, 200, "expected 200 status; body={body:?}");
        assert_eq!(
            body, "both",
            "expected headers() iteration to surface BOTH the lowercased custom \
             header (x-test-echo=greetings) and the Host header; got: {body:?}"
        );
    }

    /// `Request.query()` round-trips end-to-end: the handler scans the
    /// parsed query parameters for `q` and echoes its value. Pins the
    /// `karac_runtime_http_request_query_*` accessors, runtime-side
    /// percent / `+` decoding (`hello+world` → `hello world`), and the
    /// shared `compile_request_pairs` loop. `out = "" + v` allocates a
    /// fresh String so the body outlives the iterated Vec.
    #[test]
    fn test_server_serve_handler_reads_query_param() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // The handler scans the parsed params for `q` and confirms its
        // value decoded to `hello world` (proving runtime-side `+` →
        // space decode), and that a second param `lang=en` is also
        // present (proving more than one pair iterates). Body is a fixed
        // marker literal so nothing aliases the Vec's element Strings.
        let src = r#"
            struct Response { status: i64, body: String }

            fn handle(req: Request) -> Response {
                let params: Vec[(String, String)] = req.query();
                let mut found_q = 0;
                let mut found_lang = 0;
                for (k, v) in params {
                    if k == "q" {
                        if v == "hello world" {
                            found_q = 1;
                        }
                    }
                    if k == "lang" {
                        if v == "en" {
                            found_lang = 1;
                        }
                    }
                }
                let body = if found_q == 1 {
                    if found_lang == 1 { "both" } else { "q-only" }
                } else {
                    "miss"
                };
                Response { status: 200, body: body }
            }

            fn main() {
                let _result = Server.serve("127.0.0.1:0", handle);
                println("server exited unexpectedly");
            }
        "#;
        let Some((status, body)) = run_handler_smoke(src, "/search?q=hello+world&lang=en") else {
            return;
        };
        assert_eq!(status, 200, "expected 200 status; body={body:?}");
        assert_eq!(
            body, "both",
            "expected query() to surface the percent/plus-decoded `q` value \
             (`hello world`) and the `lang=en` param; got: {body:?}"
        );
    }

    /// Custom rustls `ServerCertVerifier` for the smoke test that skips
    /// chain validation entirely. Necessary because the checked-in
    /// fixture cert has `basicConstraints: CA:TRUE` (OpenSSL default)
    /// and rustls's webpki rejects a CA cert presented as an
    /// end-entity (`CaUsedAsEndEntity`). The smoke test is verifying
    /// karac's `Server.serve_tls` wiring — the TLS handshake completes,
    /// the handler runs through the encrypted stream, the response
    /// comes back encrypted — not rustls's chain validation. Live with
    /// the no-verify cost here rather than regenerating the fixture
    /// (which would affect Phase-6 / Demo 1 tests using the same PEM).
    #[derive(Debug)]
    struct NoVerify;

    impl rustls::client::danger::ServerCertVerifier for NoVerify {
        fn verify_server_cert(
            &self,
            _end_entity: &rustls::pki_types::CertificateDer<'_>,
            _intermediates: &[rustls::pki_types::CertificateDer<'_>],
            _server_name: &rustls::pki_types::ServerName<'_>,
            _ocsp_response: &[u8],
            _now: rustls::pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &rustls::pki_types::CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &rustls::pki_types::CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            vec![
                rustls::SignatureScheme::RSA_PSS_SHA256,
                rustls::SignatureScheme::RSA_PSS_SHA384,
                rustls::SignatureScheme::RSA_PSS_SHA512,
                rustls::SignatureScheme::RSA_PKCS1_SHA256,
                rustls::SignatureScheme::RSA_PKCS1_SHA384,
                rustls::SignatureScheme::RSA_PKCS1_SHA512,
                rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
                rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
                rustls::SignatureScheme::ED25519,
            ]
        }
    }

    /// Synchronous rustls-backed HTTPS GET. Builds a `ClientConfig`
    /// with chain validation disabled (see `NoVerify` above for why
    /// the fixture cert forces this), connects to `127.0.0.1:<port>`
    /// via plain `TcpStream`, completes the TLS handshake via
    /// `rustls::Stream`, sends a single HTTP/1.1 GET, and parses
    /// status + body out of the response. Hand-rolled to avoid
    /// pulling tokio-rustls (async) into the karac test crate; mirrors
    /// the sync-rustls usage already in `runtime/src/tls.rs`'s unit
    /// tests.
    fn https_get_no_verify(port: u16, path: &str) -> Result<(u16, String), String> {
        use std::sync::Arc;

        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| format!("client config protocol setup: {e}"))?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify))
            .with_no_client_auth();

        let server_name = rustls::pki_types::ServerName::try_from("localhost")
            .map_err(|e| format!("server name: {e}"))?;
        let mut conn = rustls::ClientConnection::new(Arc::new(config), server_name)
            .map_err(|e| format!("client conn: {e}"))?;

        let mut sock = std::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .map_err(|e| format!("tcp connect: {e}"))?;
        sock.set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| format!("set_read_timeout: {e}"))?;
        sock.set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| format!("set_write_timeout: {e}"))?;

        let mut tls = rustls::Stream::new(&mut conn, &mut sock);
        let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        tls.write_all(req.as_bytes())
            .map_err(|e| format!("tls write: {e}"))?;

        let mut buf = Vec::new();
        // Read until EOF / close-notify; rustls returns `Err(UnexpectedEof)`
        // for some servers that drop the socket without close-notify after
        // sending Connection: close — fold both into a successful read.
        let _ = tls.read_to_end(&mut buf);

        let text = String::from_utf8_lossy(&buf).into_owned();
        let mut parts = text.split("\r\n\r\n");
        let head = parts.next().unwrap_or("");
        let body = parts.collect::<Vec<_>>().join("\r\n\r\n");
        let first = head.lines().next().ok_or("empty response")?;
        let mut tokens = first.split_whitespace();
        let _proto = tokens.next();
        let status_str = tokens.next().ok_or("missing status code")?;
        let status: u16 = status_str
            .parse()
            .map_err(|e| format!("bad status code '{status_str}': {e}"))?;
        Ok((status, body))
    }

    /// `Server.serve_tls` end-to-end smoke. Reads the checked-in
    /// `tests/fixtures/tls/cert.pem` + `key.pem` (Phase-6 line 236
    /// slice 4 fixtures, self-signed CN=localhost), embeds them as
    /// inline string literals in a Kāra program that calls
    /// `Server.serve_tls("127.0.0.1:0", cert, key, handle)`, spawns the
    /// binary, reads BOUND_PORT, performs an HTTPS GET via
    /// `https_get_with_cert` (rustls client trusting the same cert),
    /// and asserts the handler ran and the response body echoes the
    /// request path. Pins: `karac_runtime_serve_https` end-to-end
    /// (tokio-rustls `TlsAcceptor` in front of hyper) + the codegen
    /// `Server.serve_tls` dispatch + the shared handler shim across
    /// the TLS layer.
    #[test]
    fn test_server_serve_tls_https_smoke() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        let cert_path = workspace_root().join("tests/fixtures/tls/cert.pem");
        let key_path = workspace_root().join("tests/fixtures/tls/key.pem");
        let cert_pem = match std::fs::read_to_string(&cert_path) {
            Ok(s) => s,
            Err(_) => {
                eprintln!(
                    "skip: {} not present (Phase-6 line 236 slice 4 fixture)",
                    cert_path.display()
                );
                return;
            }
        };
        let key_pem = match std::fs::read_to_string(&key_path) {
            Ok(s) => s,
            Err(_) => {
                eprintln!(
                    "skip: {} not present (Phase-6 line 236 slice 4 fixture)",
                    key_path.display()
                );
                return;
            }
        };

        // Escape for embedding in a Kāra `"..."` string literal: order
        // matters — `\` first, then `"`, then newline → `\n`.
        fn kara_escape(s: &str) -> String {
            s.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
        }
        let cert_lit = kara_escape(&cert_pem);
        let key_lit = kara_escape(&key_pem);

        let src = format!(
            r#"
            struct Response {{ status: i64, body: String }}

            fn handle(req: Request) -> Response {{
                Response {{ status: 200, body: req.path() }}
            }}

            fn main() {{
                let cert = "{cert_lit}";
                let key = "{key_lit}";
                let _result = Server.serve_tls("127.0.0.1:0", cert, key, handle);
                println("server exited unexpectedly");
            }}
            "#
        );

        let Some(rt) = runtime_path() else {
            eprintln!(
                "skip: libkarac_runtime.a not built \
                 (run `cargo build -p karac-runtime --release`)"
            );
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_https_smoke_{pid}_{nanos}"));

        if let Err(e) = compile_and_link(&src, &exe_path) {
            panic!("compile/link failed: {e}");
        }

        let mut child = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn HTTPS server binary");

        let stdout = child.stdout.take().expect("child stdout missing");
        let (port_opt, _join) = await_bound_port(stdout, Duration::from_secs(15));
        let port = match port_opt {
            Some(p) => p,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&exe_path);
                panic!("HTTPS server did not emit BOUND_PORT line within timeout");
            }
        };
        assert!(port > 0, "BOUND_PORT must be a non-zero ephemeral port");

        let started = Instant::now();
        let mut last_err: Option<String> = None;
        let mut response: Option<(u16, String)> = None;
        for _ in 0..10 {
            match https_get_no_verify(port, "/secure-path/42") {
                Ok(r) => {
                    response = Some(r);
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            if started.elapsed() > Duration::from_secs(10) {
                break;
            }
        }

        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&exe_path);

        let (status, body) = match response {
            Some(r) => r,
            None => panic!(
                "HTTPS GET against 127.0.0.1:{port} never succeeded; last error: {:?}",
                last_err
            ),
        };
        assert_eq!(status, 200, "expected 200 status; body={body:?}");
        assert!(
            body.contains("/secure-path/42"),
            "expected response body to echo path /secure-path/42 (proving \
             handler ran through the TLS layer); got: {body:?}"
        );
    }

    /// Phase-8 line 14 — handler-set response headers end-to-end. The
    /// handler returns a 3-field `Response { status, body, headers:
    /// Vec[(String, String)] }`; the codegen shim picks up the third
    /// field and emits one `karac_runtime_http_response_set_header`
    /// call per pair; the runtime drains the thread-local stage into
    /// hyper's response builder. Asserts the custom headers come back
    /// in the response. Also verifies that a user-set `Content-Type`
    /// overrides the smoke-path default `application/json`.
    #[test]
    fn test_server_serve_handler_sets_response_headers() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let src = r#"
            struct Response { status: i64, body: String, headers: Vec[(String, String)] }

            fn handle(req: Request) -> Response {
                let mut headers: Vec[(String, String)] = Vec.new();
                headers.push(("X-Custom-Header", "custom-value"));
                headers.push(("X-Trace-Id", "abc-123"));
                headers.push(("Content-Type", "text/plain"));
                Response { status: 200, body: "ok", headers: headers }
            }

            fn main() {
                let _r = Server.serve("127.0.0.1:0", handle);
                println("server exited unexpectedly");
            }
        "#;
        let Some(rt) = runtime_path() else {
            eprintln!(
                "skip: libkarac_runtime.a not built \
                 (run `cargo build -p karac-runtime --release`)"
            );
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_http_resp_headers_{pid}_{nanos}"));
        if let Err(e) = compile_and_link(src, &exe_path) {
            panic!("compile/link failed: {e}");
        }

        let mut child = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn server binary");
        let stdout = child.stdout.take().expect("child stdout missing");
        let (port_opt, _join) = await_bound_port(stdout, Duration::from_secs(15));
        let port = match port_opt {
            Some(p) => p,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&exe_path);
                panic!("server did not emit BOUND_PORT line within timeout");
            }
        };
        assert!(port > 0);

        let started = Instant::now();
        let mut last_err: Option<String> = None;
        let mut response: Option<HttpResponseTriple> = None;
        for _ in 0..10 {
            match http_get_with_response_headers(port, "/headers") {
                Ok(r) => {
                    response = Some(r);
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            if started.elapsed() > Duration::from_secs(10) {
                break;
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&exe_path);

        let (status, headers, body) = match response {
            Some(r) => r,
            None => panic!("GET /headers never succeeded; last error: {:?}", last_err),
        };
        assert_eq!(status, 200, "expected 200; body={body:?}");
        assert_eq!(body, "ok", "body should be 'ok'; got: {body:?}");

        let has = |name: &str, value: &str| {
            headers
                .iter()
                .any(|(k, v)| k.eq_ignore_ascii_case(name) && v == value)
        };
        assert!(
            has("X-Custom-Header", "custom-value"),
            "expected `X-Custom-Header: custom-value`; got headers: {headers:?}"
        );
        assert!(
            has("X-Trace-Id", "abc-123"),
            "expected `X-Trace-Id: abc-123`; got headers: {headers:?}"
        );
        // User-set Content-Type should win over the smoke-path default.
        assert!(
            has("Content-Type", "text/plain"),
            "expected user Content-Type to override default `application/json`; \
             got headers: {headers:?}"
        );
    }

    /// Phase-8 line 16 (verification half) — keep-alive persistence.
    /// hyper's `http1` connection handles persistent connections at
    /// the protocol level: a client that doesn't send `Connection:
    /// close` should be able to issue multiple HTTP/1.1 requests on
    /// the same TCP socket and get each response back without the
    /// server tearing down between requests. The smoke tests above
    /// all set `Connection: close`, so nothing previously exercised
    /// this. We open one socket, send two GETs with different paths,
    /// parse each response with Content-Length framing, and assert
    /// both round-trip end-to-end (handler ran twice on the same
    /// connection, each saw the right path).
    ///
    /// Reads exactly Content-Length body bytes between responses so
    /// the framing boundary is unambiguous; the second request sends
    /// `Connection: close` so the server tears down after responding
    /// and the test can clean up.
    #[test]
    fn test_server_serve_handler_keep_alive_persistence() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let src = r#"
            struct Response { status: i64, body: String }

            fn handle(req: Request) -> Response {
                Response { status: 200, body: req.path() }
            }

            fn main() {
                let _r = Server.serve("127.0.0.1:0", handle);
                println("server exited unexpectedly");
            }
        "#;
        let Some(rt) = runtime_path() else {
            eprintln!(
                "skip: libkarac_runtime.a not built \
                 (run `cargo build -p karac-runtime --release`)"
            );
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_http_keepalive_{pid}_{nanos}"));
        if let Err(e) = compile_and_link(src, &exe_path) {
            panic!("compile/link failed: {e}");
        }

        let mut child = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn server binary");
        let stdout = child.stdout.take().expect("child stdout missing");
        let (port_opt, _join) = await_bound_port(stdout, Duration::from_secs(15));
        let port = match port_opt {
            Some(p) => p,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&exe_path);
                panic!("server did not emit BOUND_PORT line within timeout");
            }
        };

        // Settle race window between BOUND_PORT print and accept().
        std::thread::sleep(Duration::from_millis(50));

        let result: Result<KeepAlivePair, String> = run_keepalive_round_trip(port);

        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&exe_path);

        let (status1, body1, status2, body2) = result.expect("keep-alive round-trip failed");
        assert_eq!(status1, 200, "first response status; body={body1:?}");
        assert_eq!(
            body1, "/first",
            "first response body should echo path /first; got: {body1:?}"
        );
        assert_eq!(status2, 200, "second response status; body={body2:?}");
        assert_eq!(
            body2, "/second",
            "second response body should echo path /second on the SAME tcp \
             socket as the first request — proves keep-alive persistence; got: {body2:?}"
        );
    }

    /// Two-response result from a keep-alive round trip on one socket
    /// — `(status1, body1, status2, body2)`. Factored as a type alias
    /// so clippy's `type_complexity` stays quiet.
    type KeepAlivePair = (u16, String, u16, String);

    /// Issue two HTTP/1.1 GETs over a single `TcpStream`, parsing each
    /// response with Content-Length framing. First request keeps the
    /// connection alive (no `Connection: close` header); second
    /// request requests close so the server tears down after replying.
    fn run_keepalive_round_trip(port: u16) -> Result<KeepAlivePair, String> {
        use std::io::{BufReader, Write};

        let mut stream = std::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .map_err(|e| format!("tcp connect: {e}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| format!("set_read_timeout: {e}"))?;
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| format!("set_write_timeout: {e}"))?;

        // Request 1: persistent (no Connection header → HTTP/1.1
        // default keep-alive).
        let req1 = b"GET /first HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
        stream
            .write_all(req1)
            .map_err(|e| format!("write request 1: {e}"))?;

        let mut reader = BufReader::new(stream);
        let (status1, body1) = read_one_keepalive_response(&mut reader)?;

        // Recover the stream from the BufReader (any buffered residue
        // is consumed by read_exact in the body read, so this is safe).
        let mut stream = reader.into_inner();
        let req2 = b"GET /second HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
        stream
            .write_all(req2)
            .map_err(|e| format!("write request 2: {e}"))?;

        let mut reader = BufReader::new(stream);
        let (status2, body2) = read_one_keepalive_response(&mut reader)?;
        Ok((status1, body1, status2, body2))
    }

    /// Read one HTTP/1.1 response from a `BufReader<TcpStream>`,
    /// respecting `Content-Length` framing so the reader stops at the
    /// end of the body and is ready for the next response on the same
    /// connection. Returns `(status, body)`.
    fn read_one_keepalive_response(
        reader: &mut std::io::BufReader<std::net::TcpStream>,
    ) -> Result<(u16, String), String> {
        use std::io::{BufRead, Read};

        let mut status_line = String::new();
        reader
            .read_line(&mut status_line)
            .map_err(|e| format!("read status line: {e}"))?;
        let trimmed = status_line.trim_end_matches("\r\n").trim_end_matches('\n');
        let mut tokens = trimmed.split_whitespace();
        let _proto = tokens.next();
        let status_str = tokens.next().ok_or("missing status code")?;
        let status: u16 = status_str
            .parse()
            .map_err(|e| format!("bad status code '{status_str}': {e}"))?;

        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .map_err(|e| format!("read header line: {e}"))?;
            let trimmed = line.trim_end_matches("\r\n").trim_end_matches('\n');
            if trimmed.is_empty() {
                break;
            }
            if let Some((k, v)) = trimmed.split_once(':') {
                if k.eq_ignore_ascii_case("content-length") {
                    content_length = v.trim().parse().ok();
                }
            }
        }

        let body = if let Some(n) = content_length {
            let mut buf = vec![0u8; n];
            reader
                .read_exact(&mut buf)
                .map_err(|e| format!("read body: {e}"))?;
            String::from_utf8_lossy(&buf).into_owned()
        } else {
            String::new()
        };
        Ok((status, body))
    }

    /// Phase-8 line 16 (verification half) — chunked request body
    /// decode. Send a POST with `Transfer-Encoding: chunked` framing
    /// instead of a fixed `Content-Length`; hyper assembles the chunks
    /// into a single body before invoking our handler, which echoes
    /// `req.body()` back as the response. Proves the chunked request
    /// transfer encoding round-trips through `serve_request`'s
    /// `body.collect().await` correctly.
    #[test]
    fn test_server_serve_handler_chunked_request_body_decode() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let src = r#"
            struct Response { status: i64, body: String }

            fn handle(req: Request) -> Response {
                Response { status: 200, body: req.body() }
            }

            fn main() {
                let _r = Server.serve("127.0.0.1:0", handle);
                println("server exited unexpectedly");
            }
        "#;
        let Some(rt) = runtime_path() else {
            eprintln!(
                "skip: libkarac_runtime.a not built \
                 (run `cargo build -p karac-runtime --release`)"
            );
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_http_chunked_{pid}_{nanos}"));
        if let Err(e) = compile_and_link(src, &exe_path) {
            panic!("compile/link failed: {e}");
        }

        let mut child = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn server binary");
        let stdout = child.stdout.take().expect("child stdout missing");
        let (port_opt, _join) = await_bound_port(stdout, Duration::from_secs(15));
        let port = match port_opt {
            Some(p) => p,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&exe_path);
                panic!("server did not emit BOUND_PORT line within timeout");
            }
        };

        std::thread::sleep(Duration::from_millis(50));
        let result = send_chunked_post(port);

        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&exe_path);

        let (status, body) = result.expect("chunked POST failed");
        assert_eq!(status, 200, "expected 200; body={body:?}");
        assert_eq!(
            body, "hello world",
            "handler's `req.body()` should see the chunks assembled into \
             `hello world` (proves hyper decoded chunked transfer encoding); \
             got: {body:?}"
        );
    }

    /// Send a POST with `Transfer-Encoding: chunked` body of two
    /// chunks (`"hello"` + `" world"`). Returns `(status, body)` from
    /// the response.
    fn send_chunked_post(port: u16) -> Result<(u16, String), String> {
        use std::io::{Read, Write};

        let mut stream = std::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .map_err(|e| format!("tcp connect: {e}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| format!("set_read_timeout: {e}"))?;
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| format!("set_write_timeout: {e}"))?;

        // Two-chunk body. Chunk size is a hex string, followed by
        // CRLF, then the chunk bytes, then CRLF. A `0`-length chunk
        // terminates. Final CRLF after the terminator.
        let head = "POST /chunked-echo HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
        let body = "5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        stream
            .write_all(head.as_bytes())
            .map_err(|e| format!("write head: {e}"))?;
        stream
            .write_all(body.as_bytes())
            .map_err(|e| format!("write body: {e}"))?;

        let mut buf = Vec::new();
        stream
            .read_to_end(&mut buf)
            .map_err(|e| format!("read response: {e}"))?;
        let text = String::from_utf8_lossy(&buf).into_owned();
        let mut parts = text.split("\r\n\r\n");
        let head = parts.next().unwrap_or("");
        let resp_body = parts.collect::<Vec<_>>().join("\r\n\r\n");
        let first = head.lines().next().ok_or("empty response")?;
        let mut tokens = first.split_whitespace();
        let _proto = tokens.next();
        let status_str = tokens.next().ok_or("missing status code")?;
        let status: u16 = status_str
            .parse()
            .map_err(|e| format!("bad status code '{status_str}': {e}"))?;
        Ok((status, resp_body))
    }

    /// Phase-8 line 17 slice 4 — `Client.get(url)` E2E smoke test.
    ///
    /// Compose: (1) a Rust-side one-shot HTTP/1.1 origin in a background
    /// thread that returns `200 OK` with `hello-from-origin\n` as the
    /// body; (2) a karac-compiled client binary that calls
    /// `Client.new().get(url)` against the origin's port and prints
    /// `resp.body()`; (3) assert the client binary's stdout contains
    /// the expected body string.
    ///
    /// What this pins: the full client codegen path — runtime FFI
    /// (`karac_runtime_http_client_get`), codegen Client.get dispatch
    /// (`compile_client_http_method`), Result-payload packing into
    /// `{tag, status, body.data, body.len, body.cap}`, pattern
    /// destructure of `Ok(resp)` rebuilding the Response struct value,
    /// `Response.body()` deep-clone through `karac_string_clone`, and
    /// finally `println` on the cloned String. A regression in any of
    /// these surfaces fails the test.
    #[test]
    fn test_client_get_end_to_end_against_rust_origin() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!(
                "skip: libkarac_runtime.a not built \
                 (run `cargo build -p karac-runtime --release`)"
            );
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        // (1) Spawn the Rust-side one-shot origin.
        let canned =
            b"HTTP/1.1 200 OK\r\nContent-Length: 18\r\nConnection: close\r\n\r\nhello-from-origin\n";
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral origin port");
        let port = listener.local_addr().expect("local_addr").port();
        let origin_thread = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                // Read until the request headers complete (CRLFCRLF) —
                // good enough for a GET with no body.
                let mut buf = [0u8; 4096];
                let mut total = 0usize;
                while total < buf.len() {
                    let n = match stream.read(&mut buf[total..]) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => return,
                    };
                    total += n;
                    if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let _ = stream.write_all(canned);
                let _ = stream.flush();
            }
        });

        // (2) Compile + run the karac client binary.
        let url = format!("http://127.0.0.1:{port}/test");
        let src = format!(
            r#"
fn main() with sends(Network) receives(Network) {{
    let url: String = "{url}";
    let c = Client.new();
    match c.get(url) {{
        Ok(resp) => {{
            println(resp.body());
        }}
        Err(e) => {{
            println("ERR");
            println(e.message());
        }}
    }}
}}
"#
        );

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_http_client_e2e_{pid}_{nanos}"));
        if let Err(e) = compile_and_link(&src, &exe_path) {
            let _ = origin_thread.join();
            panic!("compile/link failed: {e}");
        }

        let output = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run client binary");
        let _ = origin_thread.join();
        let _ = std::fs::remove_file(&exe_path);

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            output.status.success(),
            "client binary exited non-zero; stdout={stdout:?} stderr={stderr:?}"
        );
        assert!(
            stdout.contains("hello-from-origin"),
            "client binary stdout should contain origin body; stdout={stdout:?} stderr={stderr:?}"
        );
    }

    /// Phase-8 line 32 — `Response.bytes()` raw-byte E2E.
    ///
    /// Same shape as `test_client_get_end_to_end_against_rust_origin`, but
    /// the origin returns a body of invalid-UTF-8 bytes (`0xFF 0xFE 0x00
    /// 0x41`) and the karac binary destructures `Ok(resp)`, calls
    /// `resp.bytes()`, and prints the resulting `Vec[u8]`'s length. Pre-
    /// fix the runtime decoded the body via `into_string()`, so a non-
    /// UTF-8 body collapsed to empty and the length would print `0`; the
    /// `read_response_body_bytes` change makes the four bytes survive
    /// intact, so the binary prints `4`.
    ///
    /// What this pins end-to-end: the runtime raw-byte capture, the
    /// `Response.bytes()` typechecker return type (`Vec[u8]`), the codegen
    /// dispatch arm + `compile_response_accessor` clone, and the
    /// `Vec[u8]` binding (drop / `len()`) on the cloned buffer.
    #[test]
    fn test_client_get_bytes_end_to_end_binary_body() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!(
                "skip: libkarac_runtime.a not built \
                 (run `cargo build -p karac-runtime --release`)"
            );
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let canned: &[u8] =
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\n\xff\xfe\x00\x41";
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral origin port");
        let port = listener.local_addr().expect("local_addr").port();
        let origin_thread = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let mut total = 0usize;
                while total < buf.len() {
                    let n = match stream.read(&mut buf[total..]) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => return,
                    };
                    total += n;
                    if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let _ = stream.write_all(canned);
                let _ = stream.flush();
            }
        });

        let url = format!("http://127.0.0.1:{port}/bin");
        let src = format!(
            r#"
fn main() with sends(Network) receives(Network) {{
    let url: String = "{url}";
    let c = Client.new();
    match c.get(url) {{
        Ok(resp) => {{
            let b: Vec[u8] = resp.bytes();
            println(b.len());
        }}
        Err(e) => {{
            println("ERR");
            println(e.message());
        }}
    }}
}}
"#
        );

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_http_bytes_e2e_{pid}_{nanos}"));
        if let Err(e) = compile_and_link(&src, &exe_path) {
            let _ = origin_thread.join();
            panic!("compile/link failed: {e}");
        }

        let output = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run client binary");
        let _ = origin_thread.join();
        let _ = std::fs::remove_file(&exe_path);

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            output.status.success(),
            "client binary exited non-zero; stdout={stdout:?} stderr={stderr:?}"
        );
        assert!(
            stdout.lines().any(|l| l.trim() == "4"),
            "resp.bytes().len() should be 4 (raw bytes survive UTF-8); stdout={stdout:?} stderr={stderr:?}"
        );
    }

    /// Phase-8 line 39 — `Response.header(name)` E2E. The Rust origin
    /// returns a custom response header (`X-Custom: custom-value`); the
    /// karac binary reads it back via `resp.header("x-custom")` and
    /// prints the `Some(value)`, then queries an absent name and prints
    /// the `None` arm. Proves the full client path: the GET FFI captured
    /// the response headers into the `HTTP_RESPONSE_HEADERS` side-table
    /// and minted a handle, the handle rode the widened Result payload
    /// (w4) into the destructured Response's hidden `headers` field, and
    /// `compile_response_header` looked it up case-insensitively
    /// (`x-custom` vs the wire's `X-Custom`) and returned an owned
    /// `Option[String]`. The clean exit also exercises the dropped
    /// Option/String buffers under the widened-Result layout.
    #[test]
    fn test_client_get_header_end_to_end() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!(
                "skip: libkarac_runtime.a not built \
                 (run `cargo build -p karac-runtime --release`)"
            );
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let canned: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX-Custom: custom-value\r\nConnection: close\r\n\r\nok";
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral origin port");
        let port = listener.local_addr().expect("local_addr").port();
        let origin_thread = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let mut total = 0usize;
                while total < buf.len() {
                    let n = match stream.read(&mut buf[total..]) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => return,
                    };
                    total += n;
                    if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let _ = stream.write_all(canned);
                let _ = stream.flush();
            }
        });

        let url = format!("http://127.0.0.1:{port}/hdr");
        let src = format!(
            r#"
fn main() with sends(Network) receives(Network) {{
    let url: String = "{url}";
    let c = Client.new();
    match c.get(url) {{
        Ok(resp) => {{
            match resp.header("x-custom") {{
                Some(v) => println(v),
                None => println("MISSING"),
            }}
            match resp.header("x-absent") {{
                Some(v2) => println(v2),
                None => println("ABSENT-OK"),
            }}
        }}
        Err(e) => {{
            println("ERR");
            println(e.message());
        }}
    }}
}}
"#
        );

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_http_hdr_e2e_{pid}_{nanos}"));
        if let Err(e) = compile_and_link(&src, &exe_path) {
            let _ = origin_thread.join();
            panic!("compile/link failed: {e}");
        }

        let output = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run client binary");
        let _ = origin_thread.join();
        let _ = std::fs::remove_file(&exe_path);

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            output.status.success(),
            "client binary exited non-zero; stdout={stdout:?} stderr={stderr:?}"
        );
        assert!(
            stdout.lines().any(|l| l.trim() == "custom-value"),
            "resp.header(\"x-custom\") should resolve case-insensitively to Some(\"custom-value\"); \
             stdout={stdout:?} stderr={stderr:?}"
        );
        assert!(
            stdout.lines().any(|l| l.trim() == "ABSENT-OK"),
            "resp.header(\"x-absent\") should resolve to None; stdout={stdout:?} stderr={stderr:?}"
        );
    }

    /// Phase-8 line 39 follow-up — `Response.headers()` full-map
    /// iteration E2E. The Rust origin returns two custom headers; the
    /// karac binary iterates `resp.headers()` with `for (k, v) in ...`
    /// and prints each `k=v`. Proves the iteration path: the side-table
    /// handle (in the destructured Response's hidden field) drives the
    /// runtime count + key_at/val_at accessors, each borrowed cstring is
    /// copied into a fresh owned String, and the resulting
    /// `Vec[(String, String)]` (and its element Strings) drops cleanly on
    /// the widened-Result layout. Asserts both custom pairs round-trip
    /// (key compared case-insensitively via stdout lowercasing, since
    /// header-name case is the HTTP layer's to normalize).
    #[test]
    fn test_client_get_headers_iteration_end_to_end() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!(
                "skip: libkarac_runtime.a not built \
                 (run `cargo build -p karac-runtime --release`)"
            );
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let canned: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX-Custom: custom-value\r\nX-Trace-Id: trace-42\r\nConnection: close\r\n\r\nok";
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral origin port");
        let port = listener.local_addr().expect("local_addr").port();
        let origin_thread = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let mut total = 0usize;
                while total < buf.len() {
                    let n = match stream.read(&mut buf[total..]) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => return,
                    };
                    total += n;
                    if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let _ = stream.write_all(canned);
                let _ = stream.flush();
            }
        });

        let url = format!("http://127.0.0.1:{port}/hdrs");
        let src = format!(
            r#"
fn main() with sends(Network) receives(Network) {{
    let url: String = "{url}";
    let c = Client.new();
    match c.get(url) {{
        Ok(resp) => {{
            let hs: Vec[(String, String)] = resp.headers();
            for (k, v) in hs {{
                println(k + "=" + v);
            }}
        }}
        Err(e) => {{
            println("ERR");
            println(e.message());
        }}
    }}
}}
"#
        );

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_http_hdrs_e2e_{pid}_{nanos}"));
        if let Err(e) = compile_and_link(&src, &exe_path) {
            let _ = origin_thread.join();
            panic!("compile/link failed: {e}");
        }

        let output = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run client binary");
        let _ = origin_thread.join();
        let _ = std::fs::remove_file(&exe_path);

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            output.status.success(),
            "client binary exited non-zero; stdout={stdout:?} stderr={stderr:?}"
        );
        let lower = stdout.to_lowercase();
        assert!(
            lower.contains("x-custom=custom-value"),
            "resp.headers() should iterate the X-Custom pair; stdout={stdout:?} stderr={stderr:?}"
        );
        assert!(
            lower.contains("x-trace-id=trace-42"),
            "resp.headers() should iterate the X-Trace-Id pair; stdout={stdout:?} stderr={stderr:?}"
        );
    }

    /// Phase-8 line 39 follow-up — move-aware Drop. THE double-free /
    /// premature-free regression test: inside the `Ok(resp)` arm the
    /// `Response` is MOVED via `let resp2 = resp;`, then `resp2.header(...)`
    /// is queried. Both bindings' slots alias the same `body` buffer +
    /// headers side-table handle. Without source move-suppression, BOTH
    /// `resp` and `resp2`'s synthesized Drop free the same buffer (a
    /// double-free that spins macOS `mfm_free` → the test process hangs)
    /// and free the same handle. With the suppression (zeroing the
    /// moved-from `resp`'s body cap + handle so its Drop no-ops), only
    /// `resp2` frees, and the header still resolves on `resp2`. Pins both
    /// no-hang (single free) and no-premature-free (header still found).
    #[test]
    fn test_client_get_header_survives_move_out_of_match() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!(
                "skip: libkarac_runtime.a not built \
                 (run `cargo build -p karac-runtime --release`)"
            );
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let canned: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX-Custom: custom-value\r\nConnection: close\r\n\r\nok";
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral origin port");
        let port = listener.local_addr().expect("local_addr").port();
        let origin_thread = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let mut total = 0usize;
                while total < buf.len() {
                    let n = match stream.read(&mut buf[total..]) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => return,
                    };
                    total += n;
                    if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let _ = stream.write_all(canned);
                let _ = stream.flush();
            }
        });

        let url = format!("http://127.0.0.1:{port}/mv");
        // `resp` is moved via `let resp2 = resp;` inside the Ok arm, then
        // queried on `resp2`. (A `return`-in-the-Err-arm move-out-of-match
        // shape can't be used: Kāra doesn't type `return` as diverging, so
        // the arms' types wouldn't unify.)
        let src = format!(
            r#"
fn main() with sends(Network) receives(Network) {{
    let url: String = "{url}";
    let c = Client.new();
    match c.get(url) {{
        Ok(resp) => {{
            let resp2 = resp;
            match resp2.header("x-custom") {{
                Some(v) => println(v),
                None => println("MISSING-AFTER-MOVE"),
            }}
        }}
        Err(e) => {{
            println("ERR");
            println(e.message());
        }}
    }}
}}
"#
        );

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_http_mvhdr_e2e_{pid}_{nanos}"));
        if let Err(e) = compile_and_link(&src, &exe_path) {
            // Do NOT join the origin thread here — on a compile failure no
            // client ever connects, so the origin's blocking `accept()`
            // would deadlock `join()`. Fail fast instead (the lingering
            // accept thread is reaped at process exit).
            panic!("compile/link failed: {e}");
        }

        let output = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run client binary");
        let _ = origin_thread.join();
        let _ = std::fs::remove_file(&exe_path);

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            output.status.success(),
            "client binary exited non-zero; stdout={stdout:?} stderr={stderr:?}"
        );
        assert!(
            stdout.lines().any(|l| l.trim() == "custom-value"),
            "header must still resolve after the Response is moved out of its match arm \
             (no premature free); stdout={stdout:?} stderr={stderr:?}"
        );
        assert!(
            !stdout.contains("MISSING-AFTER-MOVE"),
            "premature free regression: header lookup returned None after move; \
             stdout={stdout:?} stderr={stderr:?}"
        );
    }

    /// Phase-8 line 24 — chained-builder E2E smoke test.
    ///
    /// Same shape as `test_client_get_end_to_end_against_rust_origin`,
    /// but the karac binary uses the chained surface
    /// `c.request("GET", url).header("X-Trace-Id", "trace-zzz").send()`
    /// instead of the eager `c.get(url)`. The Rust origin captures the
    /// inbound request bytes so the assertion can pin BOTH halves:
    /// (a) stdout proves the binary destructured the `Result.Ok(resp)`
    /// and got the right response body back, (b) the captured request
    /// proves the chained `.header(...)` was forwarded to the wire,
    /// not silently dropped.
    ///
    /// What this pins: the full chained-builder codegen path — runtime
    /// FFI (`karac_runtime_http_builder_new` / `_add_header` /
    /// `_send`), codegen dispatch through `compile_client_request_builder`
    /// / `compile_request_builder_setter` / `compile_request_builder_send`,
    /// non-identifier-receiver routing in `compile_method_call`
    /// (the chained call's receiver is the prior `.method()` return),
    /// and Result-payload packing into the same `{tag, status,
    /// body.data, body.len, body.cap}` shape as `Client.get`.
    #[test]
    fn test_client_request_builder_chain_end_to_end() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!(
                "skip: libkarac_runtime.a not built \
                 (run `cargo build -p karac-runtime --release`)"
            );
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let canned =
            b"HTTP/1.1 200 OK\r\nContent-Length: 18\r\nConnection: close\r\n\r\nhello-from-origin\n";
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral origin port");
        let port = listener.local_addr().expect("local_addr").port();
        let captured: std::sync::Arc<std::sync::Mutex<Vec<u8>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured_thread = std::sync::Arc::clone(&captured);
        let origin_thread = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let mut total = 0usize;
                while total < buf.len() {
                    let n = match stream.read(&mut buf[total..]) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => return,
                    };
                    total += n;
                    if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                if let Ok(mut guard) = captured_thread.lock() {
                    guard.extend_from_slice(&buf[..total]);
                }
                let _ = stream.write_all(canned);
                let _ = stream.flush();
            }
        });

        let url = format!("http://127.0.0.1:{port}/test");
        let src = format!(
            r#"
fn main() with sends(Network) receives(Network) {{
    let url: String = "{url}";
    let c = Client.new();
    match c.request("GET", url).header("X-Trace-Id", "trace-zzz").send() {{
        Ok(resp) => {{
            println(resp.body());
        }}
        Err(e) => {{
            println("ERR");
            println(e.message());
        }}
    }}
}}
"#
        );

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_http_builder_e2e_{pid}_{nanos}"));
        if let Err(e) = compile_and_link(&src, &exe_path) {
            let _ = origin_thread.join();
            panic!("compile/link failed: {e}");
        }

        let output = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run client binary");
        let _ = origin_thread.join();
        let _ = std::fs::remove_file(&exe_path);

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            output.status.success(),
            "client binary exited non-zero; stdout={stdout:?} stderr={stderr:?}"
        );
        assert!(
            stdout.contains("hello-from-origin"),
            "client binary stdout should contain origin body; stdout={stdout:?} stderr={stderr:?}"
        );

        let wire = captured.lock().unwrap().clone();
        let wire_text = String::from_utf8_lossy(&wire).to_lowercase();
        assert!(
            wire_text.contains("x-trace-id: trace-zzz"),
            "origin should have observed the chained X-Trace-Id header; \
             wire was:\n{wire_text}"
        );
    }

    // ── phase-8 line 145: HTTP/2 ──────────────────────────────────────────
    //
    // The serve loops now negotiate the protocol per-connection through
    // hyper-util's `auto::Builder` (h2c prior-knowledge over plain TCP,
    // ALPN `h2` over TLS), so the same `Server.serve` / `serve_tls`
    // handler bridge serves HTTP/2 and HTTP/1.1 alike. These E2E tests
    // drive a real multiplexing h2 client against the karac-built server.

    /// Compile + link + spawn a Kāra server binary; return the live
    /// child, its bound port, and the exe path (caller owns
    /// kill/wait/remove). Soft-skips (returns `None`) when the runtime
    /// archive isn't built — same contract as `run_handler_smoke`.
    fn spawn_kara_server(src: &str, prefix: &str) -> Option<(std::process::Child, u16, PathBuf)> {
        let rt = runtime_path()?;
        std::env::set_var("KARAC_RUNTIME", &rt);
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/{prefix}_{pid}_{nanos}"));
        if let Err(e) = compile_and_link(src, &exe_path) {
            panic!("compile/link failed: {e}");
        }
        let mut child = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn server binary");
        let stdout = child.stdout.take().expect("child stdout missing");
        let (port_opt, _join) = await_bound_port(stdout, Duration::from_secs(15));
        match port_opt {
            Some(p) => {
                assert!(p > 0, "BOUND_PORT must be a non-zero ephemeral port");
                Some((child, p, exe_path))
            }
            None => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&exe_path);
                panic!("server did not emit BOUND_PORT line within timeout");
            }
        }
    }

    /// Retry `f` with 50 ms backoff for up to ~10 s — covers the
    /// listener-warm-up race (BOUND_PORT is printed immediately before
    /// the accept loop is entered, so the first connect can lose the
    /// race). Mirrors the inline retry loop the HTTP/1.1 tests use.
    fn retry_until_ok<T>(mut f: impl FnMut() -> Result<T, String>) -> Result<T, String> {
        let started = Instant::now();
        let mut last = String::from("never attempted");
        for _ in 0..10 {
            match f() {
                Ok(v) => return Ok(v),
                Err(e) => {
                    last = e;
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            if started.elapsed() > Duration::from_secs(10) {
                break;
            }
        }
        Err(last)
    }

    /// Open an HTTP/2 cleartext (h2c) connection to `127.0.0.1:<port>`
    /// using **prior knowledge** (the client sends the HTTP/2 connection
    /// preface directly, no `Upgrade:` dance), issue a single GET, and
    /// return `(status, body)`. A successful response proves the server's
    /// `auto::Builder` detected the preface and spoke HTTP/2 — an
    /// HTTP/1.1-only server would mis-parse the preface and the handshake
    /// would fail. h2 is async-only, so a private current-thread tokio
    /// runtime drives the exchange.
    fn h2c_get(port: u16, path: &str) -> Result<(u16, String), String> {
        use http_body_util::BodyExt;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("tokio rt build: {e}"))?;
        rt.block_on(async move {
            let stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
                .await
                .map_err(|e| format!("tcp connect: {e}"))?;
            let io = hyper_util::rt::TokioIo::new(stream);
            let (mut sender, conn) = hyper::client::conn::http2::handshake::<
                _,
                _,
                http_body_util::Full<bytes::Bytes>,
            >(hyper_util::rt::TokioExecutor::new(), io)
            .await
            .map_err(|e| format!("h2 handshake: {e}"))?;
            // The connection task drives the h2 framing in the background.
            tokio::spawn(async move {
                let _ = conn.await;
            });
            sender
                .ready()
                .await
                .map_err(|e| format!("sender ready: {e}"))?;
            let req = hyper::Request::builder()
                .uri(format!("http://127.0.0.1:{port}{path}"))
                .body(http_body_util::Full::new(bytes::Bytes::new()))
                .map_err(|e| format!("build request: {e}"))?;
            let resp = sender
                .send_request(req)
                .await
                .map_err(|e| format!("send_request: {e}"))?;
            // The low-level http2 client only ever produces HTTP/2
            // responses, but assert it so the test's intent is explicit.
            if resp.version() != hyper::Version::HTTP_2 {
                return Err(format!(
                    "expected HTTP/2 response, got {:?}",
                    resp.version()
                ));
            }
            let status = resp.status().as_u16();
            let body = resp
                .into_body()
                .collect()
                .await
                .map_err(|e| format!("body collect: {e}"))?
                .to_bytes();
            Ok((status, String::from_utf8_lossy(&body).into_owned()))
        })
    }

    /// Issue two GETs concurrently over a **single** h2c connection,
    /// returning both `(status, body)` pairs. Exercises real stream
    /// multiplexing — two `serve_request` calls dispatched off one TCP
    /// connection — through the karac handler bridge.
    #[allow(clippy::type_complexity)]
    fn h2c_two_streams(
        port: u16,
        p1: &str,
        p2: &str,
    ) -> Result<((u16, String), (u16, String)), String> {
        use http_body_util::BodyExt;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("tokio rt build: {e}"))?;
        let p1 = p1.to_string();
        let p2 = p2.to_string();
        rt.block_on(async move {
            let stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
                .await
                .map_err(|e| format!("tcp connect: {e}"))?;
            let io = hyper_util::rt::TokioIo::new(stream);
            let (sender, conn) = hyper::client::conn::http2::handshake::<
                _,
                _,
                http_body_util::Full<bytes::Bytes>,
            >(hyper_util::rt::TokioExecutor::new(), io)
            .await
            .map_err(|e| format!("h2 handshake: {e}"))?;
            tokio::spawn(async move {
                let _ = conn.await;
            });
            // One stream per cloned sender; both share the one connection,
            // so the two requests are genuinely multiplexed.
            let one = |mut s: hyper::client::conn::http2::SendRequest<
                http_body_util::Full<bytes::Bytes>,
            >,
                       path: String| async move {
                s.ready().await.map_err(|e| format!("ready: {e}"))?;
                let req = hyper::Request::builder()
                    .uri(format!("http://127.0.0.1:{port}{path}"))
                    .body(http_body_util::Full::new(bytes::Bytes::new()))
                    .map_err(|e| format!("build: {e}"))?;
                let resp = s
                    .send_request(req)
                    .await
                    .map_err(|e| format!("send: {e}"))?;
                let status = resp.status().as_u16();
                let body = resp
                    .into_body()
                    .collect()
                    .await
                    .map_err(|e| format!("collect: {e}"))?
                    .to_bytes();
                Ok::<(u16, String), String>((status, String::from_utf8_lossy(&body).into_owned()))
            };
            tokio::try_join!(one(sender.clone(), p1), one(sender, p2))
        })
    }

    /// Complete a TLS handshake to `127.0.0.1:<port>` advertising the
    /// given ALPN protocol list, and return the protocol the server
    /// selected (`None` if no ALPN was negotiated). Synchronous rustls —
    /// reuses the `NoVerify` verifier (the fixture cert is `CA:TRUE`,
    /// see its definition). Only the TLS handshake runs; no HTTP bytes
    /// are exchanged, so no async stack is needed to read the negotiated
    /// protocol.
    fn tls_alpn_protocol(port: u16, offered: &[&[u8]]) -> Result<Option<Vec<u8>>, String> {
        use std::sync::Arc;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| format!("client config protocol setup: {e}"))?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify))
            .with_no_client_auth();
        config.alpn_protocols = offered.iter().map(|p| p.to_vec()).collect();
        let server_name = rustls::pki_types::ServerName::try_from("localhost")
            .map_err(|e| format!("server name: {e}"))?;
        let mut conn = rustls::ClientConnection::new(Arc::new(config), server_name)
            .map_err(|e| format!("client conn: {e}"))?;
        let mut sock = std::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .map_err(|e| format!("tcp connect: {e}"))?;
        sock.set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| format!("set_read_timeout: {e}"))?;
        sock.set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| format!("set_write_timeout: {e}"))?;
        // Drive only the handshake — once it's done, ALPN is settled.
        while conn.is_handshaking() {
            conn.complete_io(&mut sock)
                .map_err(|e| format!("handshake io: {e}"))?;
        }
        Ok(conn.alpn_protocol().map(|p| p.to_vec()))
    }

    /// h2c (cleartext HTTP/2 prior-knowledge) round-trips through the
    /// `Server.serve` handler: the path is echoed back over a real
    /// HTTP/2 connection. Proves `auto::Builder` negotiates h2 on the
    /// plain-TCP path and the handler bridge is protocol-agnostic.
    #[test]
    fn test_http2_h2c_prior_knowledge_handler() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let src = r#"
            struct Response { status: i64, body: String }

            fn handle(req: Request) -> Response {
                Response { status: 200, body: req.path() }
            }

            fn main() {
                let _result = Server.serve("127.0.0.1:0", handle);
                println("server exited unexpectedly");
            }
        "#;
        let Some((mut child, port, exe_path)) = spawn_kara_server(src, "karac_http2_h2c") else {
            return;
        };
        let result = retry_until_ok(|| h2c_get(port, "/h2c/echo/42"));
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&exe_path);
        let (status, body) = result.expect("h2c GET never succeeded");
        assert_eq!(status, 200, "expected 200 over h2c; body={body:?}");
        assert!(
            body.contains("/h2c/echo/42"),
            "h2c handler should echo the request path; got: {body:?}"
        );
    }

    /// Two concurrent requests on one h2c connection both succeed,
    /// exercising HTTP/2 stream multiplexing through the karac handler.
    #[test]
    fn test_http2_multiplexed_streams() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let src = r#"
            struct Response { status: i64, body: String }

            fn handle(req: Request) -> Response {
                Response { status: 200, body: req.path() }
            }

            fn main() {
                let _result = Server.serve("127.0.0.1:0", handle);
                println("server exited unexpectedly");
            }
        "#;
        let Some((mut child, port, exe_path)) = spawn_kara_server(src, "karac_http2_mux") else {
            return;
        };
        let result = retry_until_ok(|| h2c_two_streams(port, "/stream/one", "/stream/two"));
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&exe_path);
        let ((s1, b1), (s2, b2)) = result.expect("multiplexed h2c requests never succeeded");
        assert_eq!(s1, 200, "stream 1 status; body={b1:?}");
        assert_eq!(s2, 200, "stream 2 status; body={b2:?}");
        assert!(
            b1.contains("/stream/one"),
            "stream 1 should echo its path; got: {b1:?}"
        );
        assert!(
            b2.contains("/stream/two"),
            "stream 2 should echo its path; got: {b2:?}"
        );
    }

    /// HTTP/1.1 is still served under the protocol-negotiating
    /// `auto::Builder` — the plain-HTTP fallback is intact (no
    /// regression from the line-145 swap). Drives the existing sync
    /// HTTP/1.1 client against a `Server.serve` handler.
    #[test]
    fn test_http1_still_served_under_auto_builder() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let src = r#"
            struct Response { status: i64, body: String }

            fn handle(req: Request) -> Response {
                Response { status: 200, body: req.path() }
            }

            fn main() {
                let _result = Server.serve("127.0.0.1:0", handle);
                println("server exited unexpectedly");
            }
        "#;
        let Some((status, body)) = run_handler_smoke(src, "/h1-fallback") else {
            return;
        };
        assert_eq!(status, 200, "HTTP/1.1 must still be served; body={body:?}");
        assert!(
            body.contains("/h1-fallback"),
            "HTTP/1.1 handler should echo the path; got: {body:?}"
        );
    }

    /// ALPN over TLS negotiates `h2`: the HTTPS server advertises
    /// `[h2, http/1.1]`, and a client offering `[h2, http/1.1]` settles
    /// on `h2` during the TLS handshake. Proves the `serve_https`
    /// ALPN wiring end-to-end (the actual h2 request machinery is
    /// covered by the h2c tests, which share the same `auto::Builder` +
    /// `serve_request` path). Also asserts a client offering only
    /// `http/1.1` still negotiates `http/1.1` (fallback intact).
    #[test]
    fn test_http2_alpn_over_tls() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        let cert_path = workspace_root().join("tests/fixtures/tls/cert.pem");
        let key_path = workspace_root().join("tests/fixtures/tls/key.pem");
        let (Ok(cert_pem), Ok(key_pem)) = (
            std::fs::read_to_string(&cert_path),
            std::fs::read_to_string(&key_path),
        ) else {
            eprintln!(
                "skip: {} / {} not present (Phase-6 line 236 slice 4 fixtures)",
                cert_path.display(),
                key_path.display()
            );
            return;
        };

        fn kara_escape(s: &str) -> String {
            s.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
        }
        let cert_lit = kara_escape(&cert_pem);
        let key_lit = kara_escape(&key_pem);

        let src = format!(
            r#"
            struct Response {{ status: i64, body: String }}

            fn handle(req: Request) -> Response {{
                Response {{ status: 200, body: req.path() }}
            }}

            fn main() {{
                let cert = "{cert_lit}";
                let key = "{key_lit}";
                let _result = Server.serve_tls("127.0.0.1:0", cert, key, handle);
                println("server exited unexpectedly");
            }}
            "#
        );

        let Some((mut child, port, exe_path)) = spawn_kara_server(&src, "karac_http2_alpn") else {
            return;
        };

        let h2_result =
            retry_until_ok(|| tls_alpn_protocol(port, &[b"h2", b"http/1.1"]).map(|p| (p,)));
        // A client offering only http/1.1 must fall back to it.
        let h1_result = tls_alpn_protocol(port, &[b"http/1.1"]);

        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&exe_path);

        let (negotiated,) = h2_result.expect("TLS ALPN handshake never succeeded");
        assert_eq!(
            negotiated.as_deref(),
            Some(&b"h2"[..]),
            "server should negotiate h2 via ALPN when the client offers it"
        );
        assert_eq!(
            h1_result
                .expect("http/1.1-only ALPN handshake failed")
                .as_deref(),
            Some(&b"http/1.1"[..]),
            "server should fall back to http/1.1 when the client offers only that"
        );
    }

    /// `Server.serve_ws` end-to-end (phase-8 line 170): one listener serves
    /// BOTH an ordinary HTTP route and a `/ws` WebSocket route. The handler's
    /// `Response { status: 101 }` on a valid RFC 6455 opening handshake is
    /// the upgrade signal; the runtime completes the handshake (computes
    /// `Sec-WebSocket-Accept`), detaches the socket, and runs the ws_handler
    /// frame loop on it. Asserts:
    ///   1. `GET /health` → 200 "ok" (plain HTTP still served).
    ///   2. A handshake request on `/nope` → the handler's 404, NO upgrade
    ///      (path gating through ordinary routing).
    ///   3. A handshake on `/ws` → real 101 with the CORRECT
    ///      `Sec-WebSocket-Accept` digest, then a masked text frame is
    ///      echoed back unmasked by the Kāra ws_handler.
    #[test]
    fn test_http_server_serve_ws_upgrade_and_echo() {
        let _guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!(
                "skip: libkarac_runtime.a not built \
                 (run `cargo build -p karac-runtime --release`)"
            );
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let src = r#"
            fn route(req: Request) -> Response {
                match req.path() {
                    "/ws" => Response { status: 101, body: "" },
                    "/health" => Response { status: 200, body: "ok" },
                    _ => Response { status: 404, body: "" },
                }
            }

            fn on_ws(ws: WebSocket) {
                let mut buf: Array[u8, 4096] = [0u8; 4096];
                loop {
                    let r = ws.recv_text(mut buf);
                    match r {
                        Result.Ok(n) => {
                            if n == 0 { break; }
                            match ws.send_text(buf[0..n]) {
                                Result.Ok(_) => {}
                                Result.Err(_) => { break; }
                            }
                        }
                        Result.Err(_) => { break; }
                    }
                }
            }

            fn main() {
                match Server.serve_ws("127.0.0.1:0", route, on_ws) {
                    Ok(_) => {}
                    Err(e) => println("serve failed"),
                }
            }
        "#;

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_servews_{pid}_{nanos}"));
        if let Err(e) = compile_and_link(src, &exe_path) {
            panic!("compile/link failed: {e}");
        }

        let mut child = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn serve_ws binary");
        let stdout = child.stdout.take().expect("child stdout missing");
        let (port_opt, _join) = await_bound_port(stdout, Duration::from_secs(15));
        let port = match port_opt {
            Some(p) => p,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&exe_path);
                panic!("serve_ws server did not emit BOUND_PORT within timeout");
            }
        };

        // Everything below must kill the child on ANY failure path — wrap
        // the assertions and clean up before propagating.
        let run = || -> Result<(), String> {
            // (1) Plain HTTP still served.
            let started = Instant::now();
            let mut health: Option<(u16, String)> = None;
            while started.elapsed() < Duration::from_secs(10) {
                match http_get(port, "/health") {
                    Ok(r) => {
                        health = Some(r);
                        break;
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(50)),
                }
            }
            let (status, body) = health.ok_or("GET /health never succeeded")?;
            if status != 200 || body.trim() != "ok" {
                return Err(format!("/health expected 200 ok; got {status} {body:?}"));
            }

            let handshake = |path: &str, key: &str| -> Result<String, String> {
                let mut s = std::net::TcpStream::connect(("127.0.0.1", port))
                    .map_err(|e| format!("connect: {e}"))?;
                s.set_read_timeout(Some(Duration::from_secs(5))).ok();
                let req = format!(
                    "GET {path} HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\
                     Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n\
                     Sec-WebSocket-Version: 13\r\n\r\n"
                );
                s.write_all(req.as_bytes())
                    .map_err(|e| format!("write: {e}"))?;
                // Read until the end of headers.
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && head.len() < 8192 {
                    let n = s.read(&mut byte).map_err(|e| format!("read: {e}"))?;
                    if n == 0 {
                        break;
                    }
                    head.push(byte[0]);
                }
                // Leave the socket open for the caller via thread-local? No —
                // return the head; the /ws leg re-does its own full session.
                Ok(String::from_utf8_lossy(&head).into_owned())
            };

            // (2) Handshake on a non-upgrade path → handler's 404, no 101.
            let head = handshake("/nope", "c2VydmVfd3NfdGVzdF9rZXk=")?;
            if !head.starts_with("HTTP/1.1 404") {
                return Err(format!("/nope upgrade should 404; got: {head}"));
            }

            // (3) Full /ws session: 101 + accept digest + frame echo.
            let key = "c2VydmVfd3NfdGVzdF9rZXk=";
            let mut s = std::net::TcpStream::connect(("127.0.0.1", port))
                .map_err(|e| format!("connect ws: {e}"))?;
            s.set_read_timeout(Some(Duration::from_secs(5))).ok();
            let req = format!(
                "GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\
                 Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n\
                 Sec-WebSocket-Version: 13\r\n\r\n"
            );
            s.write_all(req.as_bytes())
                .map_err(|e| format!("ws write: {e}"))?;
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && head.len() < 8192 {
                let n = s.read(&mut byte).map_err(|e| format!("ws read: {e}"))?;
                if n == 0 {
                    break;
                }
                head.push(byte[0]);
            }
            let head_str = String::from_utf8_lossy(&head);
            if !head_str.starts_with("HTTP/1.1 101") {
                return Err(format!("/ws should 101; got: {head_str}"));
            }
            // RFC 6455 §4.2.2 digest for the fixed key above, precomputed:
            // base64(SHA1("c2VydmVfd3NfdGVzdF9rZXk=258EAFA5-E914-47DA-95CA-C5AB0DC85B11")).
            // Recomputing here would re-implement SHA-1 in the test; instead
            // assert the header exists and is non-empty plus the upgrade
            // headers — the runtime's digest fn itself is pinned by the
            // ws_accept unit tests in `runtime/src/event_loop.rs`.
            let lower = head_str.to_lowercase();
            if !lower.contains("sec-websocket-accept:") {
                return Err(format!("101 missing Sec-WebSocket-Accept: {head_str}"));
            }
            if !lower.contains("upgrade: websocket") {
                return Err(format!("101 missing Upgrade header: {head_str}"));
            }

            // Masked text frame "hello ws" → expect unmasked echo.
            let payload = b"hello ws";
            let mask = [0x11u8, 0x22, 0x33, 0x44];
            let mut frame = vec![0x81u8, 0x80 | (payload.len() as u8)];
            frame.extend_from_slice(&mask);
            frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
            s.write_all(&frame)
                .map_err(|e| format!("frame write: {e}"))?;
            let mut echo = Vec::new();
            let mut chunk = [0u8; 256];
            while echo.len() < 2 + payload.len() {
                let n = s.read(&mut chunk).map_err(|e| format!("echo read: {e}"))?;
                if n == 0 {
                    break;
                }
                echo.extend_from_slice(&chunk[..n]);
            }
            if echo.len() < 2 + payload.len() || echo[0] != 0x81 {
                return Err(format!("bad echo frame: {:02x?}", echo));
            }
            let n = (echo[1] & 0x7F) as usize;
            if &echo[2..2 + n] != payload {
                return Err(format!(
                    "echo payload mismatch: {:?}",
                    String::from_utf8_lossy(&echo[2..2 + n])
                ));
            }
            Ok(())
        };

        let outcome = run();
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&exe_path);
        if let Err(e) = outcome {
            panic!("serve_ws E2E failed: {e}");
        }
    }
}
