//! A2 slice 2b.3 — coroutine network-boundary E2E.
//!
//! Compiles a kara program in which a **free function** (`serve_one`) is a
//! network-boundary fn — it calls `listener.accept()`, a `sends(Network)
//! receives(Network)` park. With the coroutine path enabled
//! (`compile_to_object_with_coro` → `Codegen::set_coro_enabled`), `serve_one`
//! compiles as an LLVM coroutine: a ramp that registers the fd + `coro.suspend`s
//! (instead of the degenerate `emit_state_machine_poll_fn_for_key` body-splitter
//! that drops the post-park work — bug C), driven by the runtime dispatcher; the
//! caller (`main`) waits on a completion slot it passes in.
//!
//! What a green run proves (the 2b.3 acceptance gate, §6¾):
//!   1. **No hang.** `main` blocks on `park_slot_wait`; the dispatcher resumes
//!      the coroutine on fd-readiness and the body signals the slot at
//!      completion. A broken drive (caller-resume / dropped suspend) → timeout.
//!   2. **Bug C is fixed.** `serve_one` runs its post-park work (the `accept(2)`
//!      syscall on the resume edge, then `println(1)`). The degenerate path
//!      drops exactly this.
//!   3. **No UAF / clean frame lifetime.** The functional test asserts exit 0;
//!      the ASAN test (`coroutine_*_under_asan`) links `-fsanitize=address` and
//!      asserts a clean ASAN exit — the coro frame is `malloc`'d in the ramp and
//!      freed exactly once (by the dispatcher's shim `coro.destroy` on
//!      completion), never double-freed or used-after-free. (Drop *ordering*
//!      across suspends is slice 4; ASAN is necessary-but-not-sufficient there.)
//!
//! `main` is NOT a coroutine (it's the C-ABI entry, excluded from
//! `coro_fn_keys`) — its `bind` runs normally and the `BOUND_PORT=<n>` line the
//! runtime prints on an ephemeral `:0` bind drives the harness handshake, same
//! as `tests/tcp_listener.rs`.

#![cfg(feature = "llvm")]

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::{Mutex, Once, OnceLock};
    use std::time::{Duration, Instant};

    /// The straight-line single-park handler exercised by both tests. `serve_one`
    /// is a network-boundary *free* fn (parks on `accept`); with the coroutine
    /// path on it compiles as a dispatcher-driven coroutine. `println(1)` proves
    /// the post-park resume edge ran (bug C); `println(2)` proves the drive
    /// returned to `main`.
    const HANDLER_SRC: &str = r#"
        fn serve_one(listener: TcpListener) {
            let _stream = listener.accept().unwrap();
            println(1);
        }
        fn main() {
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            serve_one(listener);
            println(2);
        }
    "#;

    /// 2b.4 spawn variant: the coroutine handler is driven inside a *spawned*
    /// task (`spawn(|| ...)` → a worker-pool wrapper) rather than inline in
    /// `main`; `main` joins the handle. This services the connection
    /// functionally (the spawn wrapper runs 2b.3's coro-drive — ramp +
    /// `park_slot_wait` — on a pool worker, which the dispatcher unblocks). It
    /// is thread-blocking (one worker per concurrent handler); the
    /// density-optimal non-blocking spawn — wrapper ramps and returns, the
    /// TaskHandle's completion bound to the coroutine slot — is slice 5.
    const SPAWN_HANDLER_SRC: &str = r#"
        fn serve_one(listener: TcpListener) {
            let _stream = listener.accept().unwrap();
            println(1);
        }
        fn main() {
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            let h: TaskHandle[i64] = spawn(|| { serve_one(listener); 0 });
            let _r: i64 = h.join();
            println(2);
        }
    "#;

    /// 2b.4(b) method-handler variant: the coroutine handler is an impl method
    /// (`Server.serve`), driven through the method-call intercept's coro
    /// ramp-drive (the receiver is the ramp's `self` arg). Same dispatcher-driven
    /// slot-wait drive as the free-fn path.
    const METHOD_HANDLER_SRC: &str = r#"
        struct Acceptor { listener: TcpListener }
        impl Acceptor {
            fn run(self) {
                let _stream = self.listener.accept().unwrap();
                println(1);
            }
        }
        fn main() {
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            let a = Acceptor { listener: listener };
            a.run();
            println(2);
        }
    "#;

    /// Slice 3: a park inside a `while` LOOP. One *static* `coro.suspend` (the
    /// single `accept` call site) resumed N times across the loop back-edge —
    /// the canonical CoroSplit multi-resume case, and the real echo-handler
    /// shape. Exercises frame-residency of loop locals (`count`, `listener`)
    /// across a back-edge suspend, and the parked-record alloca being
    /// re-registered each iteration.
    const LOOP_HANDLER_SRC: &str = r#"
        fn serve_n(listener: TcpListener, n: i64) {
            let mut count: i64 = 0;
            while count < n {
                let _stream = listener.accept().unwrap();
                println(1);
                count = count + 1;
            }
            println(9);
        }
        fn main() {
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            serve_n(listener, 2);
            println(2);
        }
    "#;

    /// Slice 3: a park inside an `if` BRANCH. CoroSplit must thread the suspend
    /// through the conditional CFG and keep the post-`if` join (`println(9)`)
    /// reachable on the resume edge.
    const IF_HANDLER_SRC: &str = r#"
        fn serve_if(listener: TcpListener, doit: bool) {
            if doit {
                let _stream = listener.accept().unwrap();
                println(1);
            } else {
                println(0);
            }
            println(9);
        }
        fn main() {
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            serve_if(listener, true);
            println(2);
        }
    "#;

    /// Slice 5a — **density-optimal non-blocking spawn**. The closure body is
    /// a *tail* coroutine-handler call (`tg.spawn(|| serve_one(listener))`), so
    /// codegen routes it through `karac_runtime_spawn_coro`: the pool worker
    /// *ramps and returns* (no `park_slot_wait` blocking it), and the
    /// `TaskHandle`'s completion is bound to the coroutine's `KaracParkSlot`.
    /// The `TaskGroup`'s scope-exit drop joins the child — waiting on the
    /// coroutine slot, not a blocked worker. Contrast `SPAWN_HANDLER_SRC`,
    /// whose `{ serve_one(listener); 0 }` tail is `0` (not the coro call), so
    /// it stays on the 2b.4 thread-blocking path.
    const SPAWN_NONBLOCK_SRC: &str = r#"
        fn serve_one(listener: TcpListener) {
            let _stream = listener.accept().unwrap();
            println(1);
        }
        fn main() {
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            let mut tg = TaskGroup.new();
            tg.spawn(|| serve_one(listener));
            println(2);
        }
    "#;

    /// Slice 4: a **heap local live across a park**. `buf` (a `Vec[i64]`,
    /// stand-in for a read buffer) is allocated + filled BEFORE the `accept`
    /// park and used AFTER it (`buf.len()`), so CoroSplit must spill it into the
    /// coro frame — it is live across the suspend. On normal completion the
    /// body-end scope cleanup frees the buffer; on a destroy/cancel at the park
    /// the per-park destroy edge must free it too (else leak). This exercises
    /// the drop-across-suspend correctness slice 3 deliberately did not (slice 3
    /// handlers held only `i64` / fd locals).
    const HEAP_HANDLER_SRC: &str = r#"
        fn serve_buf(listener: TcpListener) {
            let mut buf: Vec[i64] = Vec.new();
            buf.push(1);
            buf.push(2);
            buf.push(3);
            let _stream = listener.accept().unwrap();
            println(buf.len());
        }
        fn main() {
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            serve_buf(listener);
            println(2);
        }
    "#;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    static RUNTIME_BUILT: Once = Once::new();
    static mut RUNTIME_PATH: Option<PathBuf> = None;

    #[allow(static_mut_refs)]
    fn runtime_path() -> Option<PathBuf> {
        RUNTIME_BUILT.call_once(|| {
            // Build with `test-helpers` (a superset — same as
            // `tests/park_and_wake.rs`) so concurrent runs don't strip the
            // test-helper FFIs out of the shared `libkarac_runtime.a` and break
            // sibling network tests; this E2E itself uses only always-on FFIs.
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

    /// Whether the host toolchain can produce + run an ASAN-linked executable.
    /// Mirrors `tests/memory_sanitizer.rs::asan_available`; skipping is preferred
    /// over failing so hosts without a sanitizer-capable `cc` stay green.
    fn asan_available() -> bool {
        static AVAILABLE: OnceLock<bool> = OnceLock::new();
        *AVAILABLE.get_or_init(|| {
            if std::env::var("KARAC_SKIP_ASAN_TESTS").is_ok() {
                return false;
            }
            let probe_c = "/tmp/karac_coro_asan_probe.c";
            let probe_exe = "/tmp/karac_coro_asan_probe";
            if std::fs::write(probe_c, "int main(void){return 0;}\n").is_err() {
                return false;
            }
            let link_ok = Command::new("cc")
                .args(["-fsanitize=address", probe_c, "-o", probe_exe])
                .output()
                .ok()
                .map(|o| o.status.success())
                .unwrap_or(false);
            let run_ok = link_ok
                && Command::new(probe_exe)
                    .output()
                    .ok()
                    .map(|o| o.status.success())
                    .unwrap_or(false);
            let _ = std::fs::remove_file(probe_c);
            let _ = std::fs::remove_file(probe_exe);
            run_ok
        })
    }

    /// Compile `src` with the coroutine path enabled and link it. `sanitizer`
    /// (e.g. `["-fsanitize=address"]`) routes through
    /// `link_executable_with_sanitizer`; `None` uses the plain linker.
    ///
    /// Unlike the `tcp_listener.rs` harness this populates the full
    /// network-boundary pipeline state (`callee_network_yield_effect` /
    /// `yield_points` / `state_struct_layouts` / `call_effect_subs` /
    /// `callee_purely_polymorphic_effects`) so `serve_one` lands in
    /// `state_struct_layouts` and is picked up by `coro_fn_keys`. Mirrors
    /// `tests/codegen.rs::ir_for_with_state_struct_layouts`, then routes through
    /// `compile_to_object_with_coro`.
    fn compile_link_coro(
        src: &str,
        exe_path: &Path,
        sanitizer: Option<&[&str]>,
    ) -> Result<(), String> {
        use karac::cli::{
            build_call_effect_subs_table, build_callee_network_yield_effect_table,
            build_callee_purely_polymorphic_effects_set, build_state_struct_layouts,
            build_yield_points_table,
        };
        use karac::codegen::{
            compile_to_object_with_coro, link_executable, link_executable_with_sanitizer,
        };

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

        let _ownership = karac::ownershipcheck(&parsed.program, &typed);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let obj = format!("/tmp/karac_coro_e2e_{pid}_{nanos}.o");
        compile_to_object_with_coro(&parsed.program, &obj, None, None)
            .map_err(|e| format!("codegen failed: {e}"))?;
        let link_res = match sanitizer {
            Some(flags) => link_executable_with_sanitizer(&obj, exe_path.to_str().unwrap(), flags),
            None => link_executable(&obj, exe_path.to_str().unwrap()),
        };
        link_res.map_err(|e| format!("link failed: {e}"))?;
        let _ = std::fs::remove_file(&obj);
        Ok(())
    }

    /// Drain the child's stdout in a thread: extract the `BOUND_PORT=<n>` port
    /// (via the channel) and collect every line for post-hoc marker assertions.
    #[allow(clippy::type_complexity)]
    fn spawn_stdout_reader(
        stdout: std::process::ChildStdout,
    ) -> (
        std::sync::mpsc::Receiver<u16>,
        std::thread::JoinHandle<Vec<String>>,
    ) {
        let (tx, rx) = std::sync::mpsc::channel::<u16>();
        let handle = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let mut lines = Vec::new();
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
                        lines.push(line.trim_end().to_string());
                    }
                    Err(_) => break,
                }
            }
            lines
        });
        (rx, handle)
    }

    /// Spawn `exe_path`, await its `BOUND_PORT`, connect a client to trigger the
    /// coroutine's `accept`, then wait for the binary to exit. `asan_options`,
    /// when `Some`, is set as `ASAN_OPTIONS` on the child. Returns the exit
    /// status and the collected stdout lines. Panics on the failure modes that
    /// indicate a broken drive (no port / no connect / hang).
    fn service_one_connection(
        exe_path: &Path,
        asan_options: Option<&str>,
    ) -> (std::process::ExitStatus, Vec<String>) {
        let mut cmd = Command::new(exe_path);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(opts) = asan_options {
            cmd.env("ASAN_OPTIONS", opts);
        }
        let mut child = cmd.spawn().expect("failed to spawn coro e2e binary");

        let stdout = child.stdout.take().expect("child stdout missing");
        let (rx, join) = spawn_stdout_reader(stdout);

        let port = match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(p) => p,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("binary did not emit BOUND_PORT line within 15s");
            }
        };
        assert!(port > 0, "BOUND_PORT must be a non-zero ephemeral port");

        // Connect to trigger the accept. Retry briefly to absorb the race
        // between bind's BOUND_PORT print and the coroutine's fd registration.
        let connect_started = Instant::now();
        let mut connected = false;
        for _ in 0..20 {
            if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
                connected = true;
                break;
            }
            if connect_started.elapsed() > Duration::from_secs(3) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !connected {
            let _ = child.kill();
            let _ = child.wait();
            panic!("could not connect to 127.0.0.1:{port} to trigger accept");
        }

        // The binary must exit within a timeout: the dispatcher resumed the
        // coroutine, the accept completed on the resume edge, the body signalled
        // the slot, and `main`'s slot-wait returned. A hang means the drive is
        // broken (dropped suspend / unsignalled slot).
        let wait_started = Instant::now();
        let exit_status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if wait_started.elapsed() > Duration::from_secs(15) {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!(
                            "binary did not exit within 15s after connect — the \
                             coroutine drive did not complete (dropped suspend or \
                             unsignalled completion slot)."
                        );
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("try_wait failed: {e}"),
            }
        };
        let lines = join.join().unwrap_or_default();
        (exit_status, lines)
    }

    /// Like `service_one_connection` but establishes `n` connections — for
    /// handlers whose loop services multiple connections (one static
    /// `coro.suspend` resumed across the loop back-edge). A small gap between
    /// connects lets the coroutine re-park on the next `accept`; the listener
    /// backlog absorbs any overlap. Returns the exit status + stdout lines.
    fn service_n_connections(
        exe_path: &Path,
        n: usize,
        asan_options: Option<&str>,
    ) -> (std::process::ExitStatus, Vec<String>) {
        let mut cmd = Command::new(exe_path);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(opts) = asan_options {
            cmd.env("ASAN_OPTIONS", opts);
        }
        let mut child = cmd.spawn().expect("failed to spawn coro e2e binary");
        let stdout = child.stdout.take().expect("child stdout missing");
        let (rx, join) = spawn_stdout_reader(stdout);
        let port = match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(p) => p,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("binary did not emit BOUND_PORT line within 15s");
            }
        };
        assert!(port > 0, "BOUND_PORT must be a non-zero ephemeral port");
        for k in 0..n {
            let started = Instant::now();
            let mut connected = false;
            for _ in 0..40 {
                if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
                    connected = true;
                    break;
                }
                if started.elapsed() > Duration::from_secs(3) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            if !connected {
                let _ = child.kill();
                let _ = child.wait();
                panic!("could not establish connection {k} of {n} to 127.0.0.1:{port}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let wait_started = Instant::now();
        let exit_status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if wait_started.elapsed() > Duration::from_secs(15) {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!(
                            "binary did not exit within 15s after {n} connects — \
                             the loop coroutine drive did not complete."
                        );
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("try_wait failed: {e}"),
            }
        };
        let lines = join.join().unwrap_or_default();
        (exit_status, lines)
    }

    #[test]
    fn coroutine_loop_handler_services_multiple_connections() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_loop_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(LOOP_HANDLER_SRC, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }

        let (exit_status, lines) = service_n_connections(&exe_path, 2, None);
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "loop coroutine binary exited non-success {exit_status:?}; stdout lines: {lines:?}"
        );
        // Two accept iterations each printed `1` (the resume edge ran per
        // iteration), the loop exited (`9`), and main resumed (`2`).
        let ones = lines.iter().filter(|l| *l == "1").count();
        assert!(
            ones == 2,
            "expected 2 per-iteration `1` markers (one per accept across the loop \
             back-edge), got {ones}; stdout lines: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "9") && lines.iter().any(|l| l == "2"),
            "loop did not exit + main did not resume; stdout lines: {lines:?}"
        );
    }

    #[test]
    fn coroutine_loop_handler_under_asan() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        if !asan_available() {
            eprintln!("skip: ASAN unavailable on this host");
            return;
        }
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_loop_asan_{pid}_{nanos}"));

        if let Err(e) =
            compile_link_coro(LOOP_HANDLER_SRC, &exe_path, Some(&["-fsanitize=address"]))
        {
            eprintln!("skip: ASAN compile/link failed: {e}");
            return;
        }
        let asan_options = if cfg!(target_os = "macos") {
            "abort_on_error=0:exitcode=23"
        } else {
            "detect_leaks=1:abort_on_error=0:exitcode=23"
        };

        let (exit_status, lines) = service_n_connections(&exe_path, 2, Some(asan_options));
        let _ = std::fs::remove_file(&exe_path);

        // Clean ASAN exit across two loop iterations == the coroutine frame is
        // re-registered + resumed across the back-edge with no UAF/double-free,
        // and freed exactly once at completion.
        assert!(
            exit_status.success(),
            "ASAN reported a memory error in the looping coroutine (exit \
             {exit_status:?}); stdout lines: {lines:?}"
        );
        assert!(
            lines.iter().filter(|l| *l == "1").count() == 2,
            "looping coroutine did not service both connections under ASAN; \
             stdout lines: {lines:?}"
        );
    }

    #[test]
    fn coroutine_if_branch_handler_services_connection() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_if_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(IF_HANDLER_SRC, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }

        let (exit_status, lines) = service_one_connection(&exe_path, None);
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "if-branch coroutine binary exited non-success {exit_status:?}; \
             stdout lines: {lines:?}"
        );
        // The park inside the taken `if` branch ran (`1`), the post-`if` join
        // was reached on the resume edge (`9`), and main resumed (`2`).
        assert!(
            lines.iter().any(|l| l == "1")
                && lines.iter().any(|l| l == "9")
                && lines.iter().any(|l| l == "2"),
            "if-branch coroutine did not complete; stdout lines: {lines:?}"
        );
    }

    #[test]
    fn coroutine_free_fn_services_real_connection() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
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
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_e2e_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(HANDLER_SRC, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }

        let (exit_status, lines) = service_one_connection(&exe_path, None);
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "binary exited non-success {exit_status:?} — coroutine frame lifetime \
             or drive bug. stdout lines: {lines:?}"
        );
        // Bug-C fix: the post-park work ran (resume edge), and the drive
        // returned to main.
        assert!(
            lines.iter().any(|l| l == "1"),
            "expected `1` (serve_one's post-accept println — the resume edge) in \
             stdout; got {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "2"),
            "expected `2` (main's println after the coroutine drive returned) in \
             stdout; got {lines:?}"
        );
    }

    #[test]
    fn coroutine_spawned_free_fn_services_connection() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_spawn_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(SPAWN_HANDLER_SRC, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }

        let (exit_status, lines) = service_one_connection(&exe_path, None);
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "spawned-coroutine binary exited non-success {exit_status:?}; stdout lines: {lines:?}"
        );
        // The spawned handler ran its post-park work (`1`) and `main` resumed
        // after `join()` (`2`).
        assert!(
            lines.iter().any(|l| l == "1") && lines.iter().any(|l| l == "2"),
            "spawned coroutine did not complete + join; stdout lines: {lines:?}"
        );
    }

    #[test]
    fn coroutine_nonblocking_spawn_services_connection() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_nbspawn_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(SPAWN_NONBLOCK_SRC, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }

        let (exit_status, lines) = service_one_connection(&exe_path, None);
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "non-blocking spawned-coroutine binary exited non-success {exit_status:?}; \
             stdout lines: {lines:?}"
        );
        // The handler ramped on a worker (which returned), the dispatcher drove
        // it to completion (`1`), and the TaskGroup drop joined it by waiting on
        // the coroutine slot before `main` returned. `2` is main's own line.
        assert!(
            lines.iter().any(|l| l == "1") && lines.iter().any(|l| l == "2"),
            "non-blocking spawned coroutine did not complete; stdout lines: {lines:?}"
        );
    }

    #[test]
    fn coroutine_nonblocking_spawn_under_asan() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        if !asan_available() {
            eprintln!("skip: ASAN unavailable on this host");
            return;
        }
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_nbspawn_asan_{pid}_{nanos}"));

        if let Err(e) =
            compile_link_coro(SPAWN_NONBLOCK_SRC, &exe_path, Some(&["-fsanitize=address"]))
        {
            eprintln!("skip: ASAN compile/link failed: {e}");
            return;
        }
        let asan_options = if cfg!(target_os = "macos") {
            "abort_on_error=0:exitcode=23"
        } else {
            "detect_leaks=1:abort_on_error=0:exitcode=23"
        };

        let (exit_status, lines) = service_one_connection(&exe_path, Some(asan_options));
        let _ = std::fs::remove_file(&exe_path);

        // Clean ASAN exit == the non-blocking drive's new lifetimes are sound:
        // the runtime-owned `KaracParkSlot` is freed exactly once (in
        // `task_join` after the slot fires), the coro frame is freed once by the
        // dispatcher, and the handle/env are freed once — no UAF/double-free
        // across the worker→dispatcher→joiner handoff.
        assert!(
            exit_status.success(),
            "ASAN reported a memory error in the non-blocking spawn path (exit \
             {exit_status:?}); stdout lines: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "1") && lines.iter().any(|l| l == "2"),
            "non-blocking spawned coroutine did not complete under ASAN; \
             stdout lines: {lines:?}"
        );
    }

    #[test]
    fn coroutine_method_handler_services_connection() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_method_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(METHOD_HANDLER_SRC, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }

        let (exit_status, lines) = service_one_connection(&exe_path, None);
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "method-handler coroutine binary exited non-success {exit_status:?}; \
             stdout lines: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "1") && lines.iter().any(|l| l == "2"),
            "method-handler coroutine did not complete; stdout lines: {lines:?}"
        );
    }

    /// Compile `src` with the coroutine path on and run the coro lowering
    /// passes, returning post-CoroSplit IR (the `.resume`/`.destroy` clones).
    /// Mirrors `compile_link_coro`'s full network-boundary pipeline population
    /// but routes to `compile_to_ir_with_coro_split` instead of object emission.
    fn compile_coro_split_ir(src: &str) -> Result<String, String> {
        use karac::cli::{
            build_call_effect_subs_table, build_callee_network_yield_effect_table,
            build_callee_purely_polymorphic_effects_set, build_state_struct_layouts,
            build_yield_points_table,
        };
        use karac::codegen::compile_to_ir_with_coro_split;

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
        let _ownership = karac::ownershipcheck(&parsed.program, &typed);

        // Load concurrency analysis — the CLI `karac build` path passes it, and
        // it is what surfaced the coro+auto-par interaction (a coroutine body
        // that auto-parallelizes would emit `coro.suspend` into a `__par_branch`
        // worker fn referencing the outer ramp's frame `%hdl` — invalid IR). The
        // coro_e2e harness historically passed `None`, so it could not catch
        // this; thread it through to match the real build.
        let concurrency = karac::concurrency_analyze(&parsed.program, &effects);
        compile_to_ir_with_coro_split(&parsed.program, None, Some(&concurrency))
    }

    /// Extract one `define ... @<sig_marker>...{ ... }` function body from module
    /// IR text. `sig_marker` is the `@name(`-style signature fragment (e.g.
    /// `@serve_buf.destroy(`); the slice runs from the preceding `define` to the
    /// function's closing `\n}`.
    fn extract_fn_ir<'a>(ir: &'a str, sig_marker: &str) -> Option<&'a str> {
        let at = ir.find(sig_marker)?;
        let start = ir[..at].rfind("\ndefine").map(|p| p + 1).unwrap_or(0);
        let end = ir[at..].find("\n}").map(|p| at + p + 2).unwrap_or(ir.len());
        Some(&ir[start..end])
    }

    /// Extract one `<label>:` basic block (up to the next blank-line-separated
    /// label) from a function-body IR slice.
    fn extract_block<'a>(fn_ir: &'a str, label: &str) -> Option<&'a str> {
        let marker = format!("\n{label}:");
        let start = fn_ir.find(&marker)? + 1;
        let end = fn_ir[start..]
            .find("\n\n")
            .map(|p| start + p)
            .unwrap_or(fn_ir.len());
        Some(&fn_ir[start..end])
    }

    /// Slice 4 — **destroy/cancel-edge heap drop is emitted**. A coroutine that
    /// holds a heap local (`Vec[i64] buf`) across the `accept` park must, on the
    /// per-park destroy edge (the path a future slice-5 cancel triggers), (a)
    /// deregister the fd before the frame is freed — else the dispatcher dangles
    /// a pointer into the freed frame — and (b) free the heap buffer — else a
    /// mid-flight cancel leaks it. This inspects the CoroSplit `.destroy` clone
    /// directly: today the destroy edge is emitted-but-unreached (the only
    /// runtime `coro.destroy` lands on the final suspend's free-only edge), so a
    /// structural assertion is the right gate until slice 5 wires a live cancel.
    /// The non-heap contrast handler proves the buffer free is tied to the live
    /// heap local, not emitted unconditionally.
    /// Regression: a coroutine-compiled function body must NOT be
    /// auto-parallelized. Surfaced flipping coroutines on by default for
    /// `karac build` (which loads concurrency analysis): a coroutine whose body
    /// is several independent statements would have its group lifted into a
    /// `__par_branch_*` worker fn, emitting the `coro.suspend` + frame-`%hdl`
    /// references there while `coro.begin` stayed in the outer ramp — "basic
    /// block in another function" / "does not dominate", failing module
    /// verification. Fix: `compile_function_body` falls back to sequential when
    /// `coro_ctx` is set. `compile_coro_split_ir` now loads concurrency analysis,
    /// so a regressed fix would fail this compile (verify) outright.
    #[test]
    fn coroutine_body_not_auto_parallelized() {
        const SRC: &str = r#"
            fn serve(l: TcpListener) {
                let s = l.accept().unwrap();
                println(1);
                println(2);
            }
            fn main() {
                let l = TcpListener.bind("127.0.0.1:0").unwrap();
                serve(l);
            }
        "#;
        let ir = match compile_coro_split_ir(SRC) {
            Ok(ir) => ir,
            Err(e) => panic!("coro body with auto-par-able statements failed to compile: {e}"),
        };
        // serve is a coroutine (parks) ...
        assert!(
            ir.contains("@serve.destroy(") || ir.contains("@serve.resume("),
            "serve must be coroutine-compiled (CoroSplit clones present); IR:\n{ir}"
        );
        // ... and its body was NOT sharded onto par workers.
        assert!(
            !ir.contains("@__par_branch"),
            "a coroutine body must not be auto-parallelized into par-branch \
             worker fns; IR:\n{ir}"
        );
    }

    #[test]
    fn nonblocking_spawn_uses_spawn_coro_ffi() {
        let ir = match compile_coro_split_ir(SPAWN_NONBLOCK_SRC) {
            Ok(ir) => ir,
            Err(e) => panic!("coro-split IR for non-blocking spawn failed: {e}"),
        };
        // The tail-coroutine `tg.spawn(|| serve_one(listener))` must lower to
        // the non-blocking primitive, not the thread-blocking `karac_runtime_spawn`.
        assert!(
            ir.contains("call ptr @karac_runtime_spawn_coro("),
            "non-blocking spawn must call karac_runtime_spawn_coro; IR:\n{ir}"
        );
        assert!(
            !ir.contains("call ptr @karac_runtime_spawn("),
            "tail-coroutine spawn must NOT call the blocking karac_runtime_spawn; IR:\n{ir}"
        );
        // The wrapper must ramp WITHOUT a nested `park_slot_wait` — that is the
        // worker-freeing density property. It hands the runtime-owned slot to
        // the coroutine ramp and returns; the blocking 2b.4 wrapper would call
        // `park_slot_new` + `park_slot_wait` here.
        let wrap = extract_fn_ir(&ir, "@__spawn_wrap_0(")
            .unwrap_or_else(|| panic!("no spawn wrapper in IR:\n{ir}"));
        assert!(
            !wrap.contains("@karac_runtime_park_slot_wait"),
            "the non-blocking spawn wrapper must NOT block on park_slot_wait \
             (the density property); wrapper:\n{wrap}"
        );
        assert!(
            !wrap.contains("@karac_runtime_park_slot_new"),
            "the non-blocking spawn wrapper must use the runtime-owned slot, \
             not allocate its own; wrapper:\n{wrap}"
        );
        // It does invoke the coroutine ramp (the handler runs).
        assert!(
            wrap.contains("@serve_one"),
            "the wrapper must call the coroutine ramp `serve_one`; wrapper:\n{wrap}"
        );
    }

    #[test]
    fn coroutine_heap_local_freed_on_destroy_edge() {
        let ir = match compile_coro_split_ir(HEAP_HANDLER_SRC) {
            Ok(ir) => ir,
            Err(e) => panic!("coro-split IR for heap handler failed: {e}"),
        };

        let destroy = extract_fn_ir(&ir, "@serve_buf.destroy(")
            .unwrap_or_else(|| panic!("no serve_buf.destroy clone in IR:\n{ir}"));

        // The per-park destroy edge exists and, in its own block, tears down the
        // fd registration (the dispatcher's reference into the frame).
        let destroy_block = extract_block(destroy, "kara.coro.destroy.0").unwrap_or_else(|| {
            panic!("no kara.coro.destroy.0 block in serve_buf.destroy:\n{destroy}")
        });
        assert!(
            destroy_block.contains("karac_runtime_event_loop_deregister_fd"),
            "destroy edge must deregister the fd before the frame is freed; \
             kara.coro.destroy.0 block:\n{destroy_block}"
        );

        // The live heap buffer is freed on the destroy clone (FreeVecBuffer:
        // `is_heap` cap-guard → `cleanup.free` → `free(%cleanup.data)`), ahead of
        // the shared frame free.
        assert!(
            destroy.contains("cleanup.free") && destroy.contains("cleanup.data"),
            "destroy clone must free the Vec buffer live across the park; \
             serve_buf.destroy:\n{destroy}"
        );
        assert!(
            destroy.contains("call void @free(ptr %cleanup.data)"),
            "destroy clone must call free on the Vec buffer pointer; \
             serve_buf.destroy:\n{destroy}"
        );
        // …and the frame itself is freed exactly once, after the heap drops.
        assert!(
            destroy.contains("call void @free(ptr %hdl)"),
            "destroy clone must free the coro frame; serve_buf.destroy:\n{destroy}"
        );

        // Contrast: a handler with NO heap local across the park has a destroy
        // edge that deregisters + frees the frame but emits no buffer free —
        // the drop is wired to the live heap local, not unconditional.
        let ir_no_heap = compile_coro_split_ir(HANDLER_SRC).expect("coro-split IR for non-heap");
        let destroy_no_heap = extract_fn_ir(&ir_no_heap, "@serve_one.destroy(")
            .unwrap_or_else(|| panic!("no serve_one.destroy clone:\n{ir_no_heap}"));
        assert!(
            !destroy_no_heap.contains("cleanup.free"),
            "non-heap handler's destroy edge must not emit a buffer free; \
             serve_one.destroy:\n{destroy_no_heap}"
        );
    }

    #[test]
    fn coroutine_heap_local_across_park_services_connection() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_heap_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(HEAP_HANDLER_SRC, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }

        let (exit_status, lines) = service_one_connection(&exe_path, None);
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "heap-local coroutine binary exited non-success {exit_status:?}; stdout lines: {lines:?}"
        );
        // The post-park resume edge ran and read the frame-resident buffer
        // (`buf.len()` == 3), then main resumed (`2`).
        assert!(
            lines.iter().any(|l| l == "3") && lines.iter().any(|l| l == "2"),
            "heap-local handler did not print buf.len()==3 then main's 2; \
             stdout lines: {lines:?}"
        );
    }

    #[test]
    fn coroutine_heap_local_across_park_under_asan() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        if !asan_available() {
            eprintln!("skip: ASAN unavailable on this host");
            return;
        }
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_heap_asan_{pid}_{nanos}"));

        if let Err(e) =
            compile_link_coro(HEAP_HANDLER_SRC, &exe_path, Some(&["-fsanitize=address"]))
        {
            eprintln!("skip: ASAN compile/link failed: {e}");
            return;
        }
        // On Linux, `detect_leaks=1` makes this the load-bearing leak gate: the
        // frame-resident `Vec` buffer is freed exactly once on the completion
        // path (body-end scope cleanup), with no leak and no UAF/double-free
        // against the coro frame. macOS Apple-clang ASAN lacks LeakSanitizer, so
        // there it covers UAF/double-free only.
        let asan_options = if cfg!(target_os = "macos") {
            "abort_on_error=0:exitcode=23"
        } else {
            "detect_leaks=1:abort_on_error=0:exitcode=23"
        };

        let (exit_status, lines) = service_one_connection(&exe_path, Some(asan_options));
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "ASAN reported a memory error for the heap-local-across-park coroutine \
             (exit {exit_status:?}); stdout lines: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "3") && lines.iter().any(|l| l == "2"),
            "heap-local handler did not complete under ASAN; stdout lines: {lines:?}"
        );
    }

    #[test]
    fn coroutine_free_fn_services_connection_under_asan() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        if !asan_available() {
            eprintln!("skip: ASAN unavailable on this host");
            return;
        }
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_asan_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(HANDLER_SRC, &exe_path, Some(&["-fsanitize=address"])) {
            // Runtime archive not ASAN-linkable on this host — skip, don't fail.
            eprintln!("skip: ASAN compile/link failed: {e}");
            return;
        }

        // macOS Apple-clang ASAN lacks LeakSanitizer (`detect_leaks` unsupported)
        // — keep UAF / double-free / heap-overflow coverage there; enable the
        // leak arm on Linux. `exitcode=23` makes an ASAN error a non-success
        // exit. Same posture as `tests/memory_sanitizer.rs`.
        let asan_options = if cfg!(target_os = "macos") {
            "abort_on_error=0:exitcode=23"
        } else {
            "detect_leaks=1:abort_on_error=0:exitcode=23"
        };

        let (exit_status, lines) = service_one_connection(&exe_path, Some(asan_options));
        let _ = std::fs::remove_file(&exe_path);

        // Clean ASAN exit == the coroutine frame lifetime is sound: the frame is
        // freed exactly once (dispatcher shim `coro.destroy` on completion), the
        // caller-owned slot is freed exactly once, no UAF on the resume/destroy
        // edges. An ASAN error would exit 23 (or signal) → !success().
        assert!(
            exit_status.success(),
            "ASAN reported a memory error in the coroutine path (exit {exit_status:?}); \
             stdout lines: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "1") && lines.iter().any(|l| l == "2"),
            "coroutine did not complete under ASAN; stdout lines: {lines:?}"
        );
    }
}
