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
