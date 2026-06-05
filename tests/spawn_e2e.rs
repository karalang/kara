//! Phase 6 line 218 slice 7 — end-to-end `spawn()` / `TaskGroup`
//! smoke test.
//!
//! Compiles a kara program that exercises the demo-1-shape pattern
//! from `design.md § Explicit Concurrency`:
//!
//! ```kara
//! let mut tg = TaskGroup.new();
//! tg.spawn(|| worker(...));    // N times
//! // tg drops here — implicit join barrier on all N spawned tasks
//! ```
//!
//! Runs the binary and asserts: (a) it exits with success and (b)
//! every spawned worker's println output reaches stdout. Together
//! these prove the slice 3 → 4 → 5 stack composes end-to-end:
//! slice-3 runtime dispatches the closure to the worker pool, slice-4
//! codegen synthesizes the `SpawnFn` wrapper + populates env + calls
//! `karac_runtime_spawn` and registers with the TaskGroup, slice-5
//! `@TaskGroup.drop` joins every registered child before main()
//! returns.
//!
//! **Subprocess + output-assertion pattern** mirrors
//! `tests/tcp_listener.rs` minus the BOUND_PORT/connect ceremony —
//! no network surface here, so the program runs to completion under
//! its own steam and the harness just inspects exit status + stdout.
//!
//! **What this does NOT exercise** (deferred to follow-on slices):
//! - Network-yielding closures (state-machine integration with spawn
//!   lands once a real demo-1 pass through the bind→accept→spawn
//!   loop is wired — composes against the same wrapper signature).
//! - `.join()` extracting non-i64 return types (slice 4's T-binding
//!   gap; TaskGroup.drop discards results so this slice doesn't trip
//!   the gap).
//! - Fail-fast cancel propagation (slice 5b; v1's panic = abort
//!   posture aborts the process before drop runs anyway).

#[cfg(all(unix, feature = "llvm"))]
mod spawn_e2e_tests {
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::{Mutex, Once};
    use std::time::Duration;

    static SPAWN_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    static RUNTIME_BUILT: Once = Once::new();
    static mut RUNTIME_PATH: Option<PathBuf> = None;

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
        let obj = format!("/tmp/karac_spawn_e2e_{pid}_{nanos}.o");
        compile_to_object_with_options(&parsed.program, &obj, None, None, None, None)
            .map_err(|e| format!("codegen failed: {e}"))?;
        link_executable(&obj, exe_path.to_str().unwrap())
            .map_err(|e| format!("link failed: {e}"))?;
        let _ = std::fs::remove_file(&obj);
        Ok(())
    }

    /// Run a compiled binary with a hard timeout. Returns (exit_status,
    /// stdout, stderr). Panics on timeout or spawn failure — the slice
    /// 7 contract is "binary completes in bounded time".
    fn run_with_timeout(
        exe: &Path,
        timeout: Duration,
    ) -> (std::process::ExitStatus, String, String) {
        use std::process::Stdio;
        use std::time::Instant;

        let mut child = Command::new(exe)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn child");

        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) => {
                    if start.elapsed() > timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!(
                            "binary did not exit within {:?} — \
                             spawn/TaskGroup.drop may have hung",
                            timeout
                        );
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("try_wait failed: {e}"),
            }
        };

        let mut stdout_buf = String::new();
        let mut stderr_buf = String::new();
        if let Some(mut s) = child.stdout {
            use std::io::Read;
            let _ = s.read_to_string(&mut stdout_buf);
        }
        if let Some(mut s) = child.stderr {
            use std::io::Read;
            let _ = s.read_to_string(&mut stderr_buf);
        }
        (status, stdout_buf, stderr_buf)
    }

    /// **Slice 7 primary deliverable.** A single `tg.spawn(closure)`
    /// call inside a `TaskGroup` scope. The spawned worker prints `42`;
    /// scope-exit drop on `tg` must join the worker before `main`
    /// returns, so the binary's exit-status `0` and stdout containing
    /// `42` together pin: (a) codegen lowered `tg.spawn(closure)` to
    /// the runtime spawn FFI; (b) the worker actually ran on the pool;
    /// (c) `@TaskGroup.drop` waited for the child before main exited.
    #[test]
    fn test_spawn_single_task_runs_and_joins_via_taskgroup_drop() {
        let _guard = SPAWN_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!(
                "skip: libkarac_runtime.a not built \
                 (run `cargo build -p karac-runtime --release`)"
            );
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let src = r#"
            fn worker() {
                println(42);
            }
            fn main() {
                let mut tg = TaskGroup.new();
                tg.spawn(|| worker());
            }
        "#;

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_spawn_e2e_single_{pid}_{nanos}"));

        if let Err(e) = compile_and_link(src, &exe_path) {
            let _ = std::fs::remove_file(&exe_path);
            panic!("compile/link failed: {e}");
        }

        let (status, stdout, stderr) = run_with_timeout(&exe_path, Duration::from_secs(10));
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            status.success(),
            "binary exited non-success {status:?}; stdout=`{stdout}` stderr=`{stderr}`"
        );
        assert!(
            stdout.contains("42"),
            "expected worker's `println(42)` in stdout; got `{stdout}`"
        );
    }

    /// Fan-out shape: 5 concurrent spawns, each prints its id. The
    /// TaskGroup's scope-exit drop joins all 5 before main returns,
    /// so every printed line must appear before EOF.
    ///
    /// Ordering is not asserted — workers may print in any order.
    /// Membership is asserted by parsing each non-empty line as an
    /// i64 and checking the set against {1..=5}.
    #[test]
    fn test_spawn_fan_out_five_tasks_all_join() {
        let _guard = SPAWN_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!(
                "skip: libkarac_runtime.a not built \
                 (run `cargo build -p karac-runtime --release`)"
            );
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        // Each closure captures a distinct constant via a no-arg
        // closure body. The 5 numbers exercise the per-call capture
        // path (free-vars list is empty for each closure since the
        // literal `1`, `2`, ... aren't free variables — they're
        // inline constants).
        let src = r#"
            fn worker(n: i64) {
                println(n);
            }
            fn main() {
                let mut tg = TaskGroup.new();
                tg.spawn(|| worker(1));
                tg.spawn(|| worker(2));
                tg.spawn(|| worker(3));
                tg.spawn(|| worker(4));
                tg.spawn(|| worker(5));
            }
        "#;

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_spawn_e2e_fanout_{pid}_{nanos}"));

        if let Err(e) = compile_and_link(src, &exe_path) {
            let _ = std::fs::remove_file(&exe_path);
            panic!("compile/link failed: {e}");
        }

        let (status, stdout, stderr) = run_with_timeout(&exe_path, Duration::from_secs(15));
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            status.success(),
            "fan-out binary exited non-success {status:?}; \
             stdout=`{stdout}` stderr=`{stderr}`"
        );
        let printed: HashSet<i64> = stdout
            .lines()
            .filter_map(|l| l.trim().parse::<i64>().ok())
            .collect();
        let expected: HashSet<i64> = (1..=5).collect();
        assert_eq!(
            printed, expected,
            "all 5 spawned workers must print before TaskGroup.drop returns; \
             got `{stdout}`"
        );
    }

    /// **Phase 6 line 218 slice 6 — cross-task-safe boundary enforced
    /// in the BUILD pipeline (defense-in-depth regression guard).**
    ///
    /// Slice 6's integration is delivered by the line-170 entry's slice
    /// 3a: the `spawn` / `TaskGroup.spawn` cross-task-safe check fires at
    /// the *typechecker* layer (`src/typechecker/cross_task_check.rs`),
    /// which strictly subsumes the originally-imagined codegen-side hook
    /// — unsafe captures are rejected before codegen ever runs. The 8
    /// existing slice-3a tests assert this via the typechecker unit path;
    /// this test pins the same rejection through the *exact* phase chain
    /// `compile_and_link` runs (parse → resolve → **typecheck** → lower →
    /// codegen → link), so a future refactor that moved the check to a
    /// pass the build path skips (cf. the ownership-pass-is-skipped-by-
    /// `build` footgun the line-390 entry documents) would fail here.
    ///
    /// A `TaskGroup.spawn(|| use_it(c))` capturing a `shared struct`
    /// must be rejected at the typecheck phase — `compile_and_link`
    /// returns `Err` carrying the phase tag + the `E_NOT_CROSS_TASK`
    /// boundary message — and NO binary may be emitted.
    #[test]
    fn test_spawn_unsafe_capture_rejected_before_codegen() {
        let _guard = SPAWN_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // No runtime archive needed: rejection happens at typecheck,
        // before codegen/link, so this runs regardless of archive state.
        let src = r#"
            shared struct Cache { n: i64 }
            fn use_it(c: Cache) -> i64 { 0 }
            fn main() {
                let c = Cache { n: 1 };
                let mut tg = TaskGroup.new();
                tg.spawn(|| use_it(c));
            }
        "#;

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_spawn_e2e_unsafe_{pid}_{nanos}"));

        let result = compile_and_link(src, &exe_path);
        let emitted = exe_path.exists();
        let _ = std::fs::remove_file(&exe_path);

        let err = result.expect_err(
            "build pipeline must reject a spawn capturing a `shared struct` \
             before codegen — got a successful compile/link instead",
        );
        assert!(
            err.contains("typecheck errors"),
            "rejection must come from the typecheck phase of the build \
             pipeline (so `build` can't bypass it); got: `{err}`"
        );
        assert!(
            err.contains("E_NOT_CROSS_TASK") && err.contains("task boundary"),
            "rejection must be the cross-task-safe boundary diagnostic; \
             got: `{err}`"
        );
        assert!(
            !emitted,
            "no binary may be emitted for a rejected cross-task-unsafe \
             spawn capture"
        );
    }
}
