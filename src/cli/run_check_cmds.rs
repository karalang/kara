//! `karac run` / `check` (+ profiles and per-target checks) — the
//! interpret/JIT executor drivers and the check pipeline reporting.
//!
//! Extracted verbatim from `cli.rs` (structural-debt extraction, slice 3).

use super::*;

// ── Commands ────────────────────────────────────────────────────

pub(super) fn cmd_run_example(
    name: &str,
    output: OutputMode,
    sequential: bool,
    lint_overrides: crate::lints::CliLintOverrides,
) {
    // Try single-file form first, then project-style directory form.
    let single_file = format!("examples/{name}.kara");
    let dir_entry = format!("examples/{name}/src/main.kara");
    let path = if std::path::Path::new(&single_file).exists() {
        single_file
    } else if std::path::Path::new(&dir_entry).exists() {
        dir_entry
    } else {
        eprintln!("error: example '{name}' not found");
        eprintln!("  looked for: {single_file}");
        eprintln!("              {dir_entry}");
        list_available_examples();
        process::exit(1);
    };
    // `karac run --example NAME` runs an example file out of the
    // examples/ directory; it has no `kara.toml`-style project root,
    // so manifest discovery is intentionally skipped.
    // `interp = false`: `run --example` uses the JIT-default backend too (6c),
    // with the same `--interp`/`KARAC_RUN_JIT=0` escape hatches honored inside.
    // `run --example` takes no trailing program arguments — the examples it
    // runs are self-contained demos, so the program argv is just the script.
    super::run_v2::cmd_run_either(
        &path,
        output,
        sequential,
        None,
        true,
        lint_overrides,
        None,
        false,
        &[],
    );
}

pub(super) fn list_available_examples() {
    let names = walker::walk_examples(std::path::Path::new("."));
    if names.is_empty() {
        return;
    }
    eprintln!("available examples:");
    for n in &names {
        eprintln!("  {n}");
    }
}

/// Best-effort dependency walks for the lenient `karac run` path: resolve
/// the manifest's `[dependencies]` and walk each path-dep, returning an
/// empty list on any failure (no diagnostics — the strict build path owns
/// error reporting, and `run`'s resolver pass surfaces unknown-module
/// diagnostics naturally when dep modules are absent).
/// The dependencies a strict build already resolved (fetched git and
/// registry ones included), which the package-wide check then walks instead
/// of its own path-only walk (B-2026-10-22-14).
static RESOLVED_DEP_WALKS: std::sync::Mutex<Option<Vec<module::DepPackageWalk>>> =
    std::sync::Mutex::new(None);

pub(super) fn install_resolved_dep_walks(walks: Vec<module::DepPackageWalk>) {
    *RESOLVED_DEP_WALKS.lock().unwrap_or_else(|e| e.into_inner()) = Some(walks);
}

pub(super) fn quiet_dep_package_walks(root: &std::path::Path) -> Vec<module::DepPackageWalk> {
    if let Some(walks) = RESOLVED_DEP_WALKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
    {
        return walks.clone();
    }
    let Ok(mf) = manifest::load_from_root(root) else {
        return Vec::new();
    };
    if mf.dependencies.is_empty() {
        return Vec::new();
    }
    let loader = crate::dep_graph::FsLoader;
    let options = crate::dep_graph::DepGraphOptions {
        offline_root: None,
        include_dev_deps: false,
        // The lenient `karac run` walk stays path-dep-only by design: it is
        // best-effort (empty on any failure) and must not perform network I/O.
        // Registry and git fetch are activated on the strict `karac build` /
        // `karac test` path (`run_dep_resolution`); a registry or git dep here
        // still surfaces its unsupported diagnostic from the resolver, which
        // this quiet walk swallows.
        registry_provider: None,
        git_provider: None,
        // Path-dep-only lenient walk — no lockfile pinning here.
        pins: None,
    };
    let Ok(graph) = crate::dep_graph::build_dep_graph_with_options(root, mf, &loader, options)
    else {
        return Vec::new();
    };
    let active = crate::dep_resolver::active_toolchain_version();
    let Ok(resolution) = crate::dep_resolver::resolve(&graph, &active) else {
        return Vec::new();
    };
    let Ok(names) = super::build_cmds::dep_import_names(root, &resolution) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for pkg in resolution.packages.values() {
        let crate::dep_resolver::ResolvedSource::Path(dep_root) = &pkg.source else {
            continue;
        };
        let Ok(walked) = walker::walk_project(dep_root, WalkerOpts::default()) else {
            return Vec::new();
        };
        if walked.entry != walker::EntryKind::Lib {
            return Vec::new();
        }
        let (import_name, renames) = names
            .get(&pkg.name)
            .cloned()
            .unwrap_or_else(|| (manifest::default_import_name(&pkg.name), Vec::new()));
        out.push(module::DepPackageWalk {
            name: pkg.name.clone(),
            import_name,
            renames,
            walked,
        });
    }
    out
}

/// If `filename` is the entry of a multi-module project, build the merged
/// super-program so `karac run` sees every sibling module's items (GAP-W3 —
/// previously the interpreter only registered the entry file's items, so
/// cross-module calls failed at runtime despite resolving + typechecking).
/// Returns `None` for a single-file script or a one-module project (the caller
/// keeps the single-file fast path), so behavior is unchanged outside real
/// multi-module projects. Canonicalizes the entry first so the canonical
/// invocation `karac run src/main.kara` (relative path) discovers the root —
/// `discover_project_root` can't walk up a bare relative `src`.
/// Refuse to run a package whose module-aware check fails.
///
/// `karac run` executes a merged super-program (see
/// [`try_build_run_super_program`]), which is exactly why it needs this: the
/// merge flattens away the module boundaries that carry visibility, so the
/// run path's own resolve cannot see a `private` item being imported across
/// directories. Without the gate, `karac run` was the one surface that
/// executed a program `karac build` rejects.
///
/// Diagnostics are rendered exactly as the build renders them, so the two
/// commands are quotable against each other.
///
/// This returns the refusal, for [`render_package_refusal`]; a refusal the v2
/// check of the combined source makes too is left to that check
/// (`run_v2::combine`, B-2026-10-25-4).
pub(super) fn package_check_refusal(
    root: &std::path::Path,
) -> Option<super::build_cmds::PackageCheck> {
    let mut lints = crate::lints::CliLintOverrides::default();
    if let Ok(mf) = manifest::load_from_root(root) {
        lints.apply_manifest_lints(&mf.lints);
    }
    // A walk/tree failure is NOT fatal here: the single-file path below will
    // surface its own diagnostic against the entry file, and this gate exists
    // to add errors the run path cannot see, never to invent a new way for
    // `karac run` to fail on a package the build handles.
    let pc = super::build_cmds::package_check(root, &lints, crate::target::active_target()).ok()?;
    pc.has_errors().then_some(pc)
}

/// [`package_check_refusal`] for a script inside the package at `root` but
/// outside its `src/`: the check of the script's own program, not of the
/// package's modules (B-2026-11-13-30).
pub(super) fn script_check_refusal(
    root: &std::path::Path,
    script: &std::path::Path,
) -> Option<super::build_cmds::PackageCheck> {
    let mut lints = crate::lints::CliLintOverrides::default();
    if let Ok(mf) = manifest::load_from_root(root) {
        lints.apply_manifest_lints(&mf.lints);
    }
    let pc = super::build_cmds::script_check(root, script, &lints).ok()?;
    pc.has_errors().then_some(pc)
}

/// The files of a package's modules, as reports name them.
pub(super) fn package_files(tree: &crate::module::ProgramTree) -> Vec<String> {
    tree.modules
        .iter()
        .filter(|m| !m.is_synthetic)
        .map(|m| super::project_v2::shown(&m.file))
        .collect()
}

/// Render a refusing [`package_check_refusal`] as the build renders it, and
/// exit 1.
pub(super) fn render_package_refusal(
    pc: &super::build_cmds::PackageCheck,
    output: OutputMode,
) -> ! {
    match output {
        // The batch document and the event stream a pipeline gives, ids and
        // phases included (design.md § 17, B-2026-11-05-26).
        OutputMode::Json | OutputMode::Jsonl => {
            let mut diags: Vec<String> = Vec::new();
            diags.extend(super::build_cmds::parse_errors_json(&pc.parse_errors));
            diags.extend(super::build_cmds::cycles_json(&pc.cycles, &pc.tree));
            diags.extend(super::build_cmds::resolve_errors_json(&pc.resolve_errors));
            diags.extend(super::build_cmds::type_errors_json(&pc.type_errors));
            if let Some(f) = &pc.late_failure {
                diags.extend(super::build_cmds::late_failure_json(f));
            }
            emit_found_entries(&diags, &package_files(&pc.tree), output, None);
        }
        OutputMode::Text => {
            super::build_cmds::print_parse_errors_text(&pc.parse_errors);
            super::build_cmds::print_cycles_text(&pc.cycles, &pc.tree);
            super::build_cmds::print_resolve_errors_text(&pc.resolve_errors);
            super::build_cmds::print_type_errors_text(&pc.type_errors);
            // B-2026-08-27-27 — `has_errors()` now counts the flattened
            // late-phase failure, so this arm must RENDER it. Without the
            // line, `karac run` on a project with an undeclared effect exits
            // 1 having printed nothing at all.
            if let Some(f) = &pc.late_failure {
                super::build_cmds::print_late_failure_text(f);
            }
        }
    }
    process::exit(1);
}

/// Forward the signals `karac run` can catch to the `karac_jit_runner` child —
/// B-2026-09-05-24.
///
/// `karac run` is the DEFAULT executor, so this is the path a user takes to run
/// a program interactively — and until this guard existed, signalling `karac
/// run` killed the wrapper and left the JIT'd program running. Ctrl-C on a Kāra
/// program that loops forever returned the shell prompt while the program kept
/// burning a core, under a process name (`karac_jit_runner`) that names neither
/// the program nor the command the user typed.
///
/// Forwarding rather than killing: the child's death-by-signal is what
/// [`exit_code_of`] turns into the shell's `128 + signal`, so an interrupted
/// `karac run` reports the same status an interrupted AOT binary would. The
/// parent then falls out of `wait()` naturally and its ordinary cleanup — the
/// temp IR file — runs on the way out, which a handler that exited on the spot
/// would have skipped.
///
/// Escalation, and why it is not optional: a child that does not die on the
/// first delivery would otherwise leave the parent blocked in `wait()` with the
/// user's Ctrl-C swallowed — strictly worse than the bug being fixed. So the
/// SECOND delivery of the same signal (and any delivery with no child recorded)
/// restores the default disposition and re-raises, which is also what makes the
/// pre-spawn window safe: the handlers are armed BEFORE the fork so no signal
/// is ever silently dropped, and until [`attach`](Forwarder::attach) records a
/// pid they simply do what the kernel would have done anyway.
///
/// SIGKILL and SIGSTOP are absent because they cannot be caught. The child's
/// own `getppid` watchdog covers those (see `karac_jit_runner`'s
/// `watch_for_parent_death`), and covers the instructions between `spawn()`
/// returning and `attach` storing the pid.
#[cfg(all(feature = "llvm", unix))]
mod jit_child_signals {
    use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

    /// The signals a parent can act on that would otherwise strand the child.
    /// All three default to terminating this process.
    const FORWARDED: [i32; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

    /// The runner's pid, or 0 before the spawn / after the wait.
    static CHILD_PID: AtomicI32 = AtomicI32::new(0);
    /// One bit per signal number already forwarded once, so a repeat escalates.
    static SEEN: AtomicU32 = AtomicU32::new(0);

    /// Async-signal-safe: an atomic read-modify-write plus `kill`, `signal` and
    /// `raise`, all three of which POSIX lists as safe to call from a handler.
    extern "C" fn handler(sig: i32) {
        let pid = CHILD_PID.load(Ordering::SeqCst);
        let bit = 1u32 << (sig as u32 & 31);
        let first = SEEN.fetch_or(bit, Ordering::SeqCst) & bit == 0;
        if pid > 0 && first {
            // SAFETY: `kill` on a pid we spawned and have not yet reaped.
            unsafe { libc::kill(pid, sig) };
            return;
        }
        // SAFETY: restoring the default disposition and re-raising is the
        // documented way to take the default action from inside a handler.
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }

    /// Armed for the lifetime of one child; restores the default dispositions
    /// on drop so a later signal to `karac` is handled normally again.
    pub(super) struct Forwarder;

    impl Forwarder {
        /// Install the handlers. Call BEFORE spawning, so the window in which a
        /// signal would take the default action while a child already exists is
        /// as short as the code between `spawn()` and [`Self::attach`].
        pub(super) fn arm() -> Self {
            CHILD_PID.store(0, Ordering::SeqCst);
            SEEN.store(0, Ordering::SeqCst);
            for sig in FORWARDED {
                // SAFETY: a zeroed `sigaction` with an explicitly set handler,
                // empty mask and `SA_RESTART` is the standard installation.
                unsafe {
                    let mut sa: libc::sigaction = std::mem::zeroed();
                    sa.sa_sigaction = handler as *const () as libc::sighandler_t;
                    libc::sigemptyset(&mut sa.sa_mask);
                    // Restart interrupted syscalls: `Child::wait` retries on
                    // EINTR anyway, but the JIT'd program is not yet running
                    // and nothing here wants a spurious short read.
                    sa.sa_flags = libc::SA_RESTART;
                    libc::sigaction(sig, &sa, std::ptr::null_mut());
                }
            }
            Forwarder
        }

        pub(super) fn attach(&self, child_pid: u32) {
            CHILD_PID.store(child_pid as i32, Ordering::SeqCst);
        }
    }

    impl Drop for Forwarder {
        fn drop(&mut self) {
            CHILD_PID.store(0, Ordering::SeqCst);
            for sig in FORWARDED {
                // SAFETY: restoring the default disposition of a standard signal.
                unsafe { libc::signal(sig, libc::SIG_DFL) };
            }
        }
    }
}

/// Non-Unix stand-in: there is no signal to forward, so the guard does nothing.
#[cfg(all(feature = "llvm", not(unix)))]
mod jit_child_signals {
    pub(super) struct Forwarder;

    impl Forwarder {
        pub(super) fn arm() -> Self {
            Forwarder
        }
        pub(super) fn attach(&self, _child_pid: u32) {}
    }

    // Present so the explicit `drop(forwarder)` at the call site reads as the
    // deliberate scope marker it is on every platform, rather than tripping
    // `clippy::drop_non_drop` here.
    impl Drop for Forwarder {
        fn drop(&mut self) {}
    }
}

/// How a [`run_ir_via_jit_subprocess_with`] run ended.
#[cfg(feature = "llvm")]
pub(super) struct JitRun {
    /// The runner's exit code, a signal death as `128 + signal`.
    pub code: i32,
    /// What the program wrote to stderr, when the caller asked to capture it.
    pub stderr: Option<String>,
    /// The run passed its deadline and the runner was killed.
    pub timed_out: bool,
}

/// Run a codegen-emitted IR module through the `karac_jit_runner` one-shot
/// subprocess, with a wall-clock deadline after which the runner is killed, and the program's stderr captured instead of inherited
/// when `capture_stderr` (v2 `karac run --output=json` turns its runtime
/// reports into JSON, B-2026-10-22-12). `quiet_stdout` discards stdout and
/// `env` adds to the runner's environment (v2 `karac test` keeps its JSONL
/// stream clean of a case's output and names the case to run there,
/// B-2026-10-22-13).
#[cfg(feature = "llvm")]
pub(super) fn run_ir_via_jit_subprocess_with(
    ir: &str,
    program_argv: &[String],
    timeout: Option<std::time::Duration>,
    capture_stderr: bool,
    quiet_stdout: bool,
    env: &[(&str, &str)],
) -> JitRun {
    let ir_path = jit_handoff_stem().with_extension("ll");
    if let Err(e) = std::fs::write(&ir_path, ir) {
        eprintln!(
            "error: could not write JIT IR to {}: {e}",
            ir_path.display()
        );
        return JitRun {
            code: 1,
            stderr: None,
            timed_out: false,
        };
    }
    run_files_via_jit_subprocess_with(
        &[ir_path],
        program_argv,
        timeout,
        capture_stderr,
        quiet_stdout,
        env,
    )
}

/// Where this process's JIT handoff files go: `<tmp>/karac_run_<pid>_jit`,
/// with `.ll` for IR and `.cgu<n>.o` for a split program's objects.
#[cfg(feature = "llvm")]
pub(super) fn jit_handoff_stem() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("karac_run_{}_jit", std::process::id()))
}

/// [`run_ir_via_jit_subprocess_with`] for what
/// `backend::mir::compile_mir_for_jit` produced: the IR, or the objects of
/// a split program (B-2026-11-10-5).
#[cfg(feature = "llvm")]
pub(super) fn run_code_via_jit_subprocess_with(
    code: crate::backend::mir::JitCode,
    program_argv: &[String],
    timeout: Option<std::time::Duration>,
    capture_stderr: bool,
    quiet_stdout: bool,
    env: &[(&str, &str)],
) -> JitRun {
    match code {
        crate::backend::mir::JitCode::Ir(ir) => run_ir_via_jit_subprocess_with(
            &ir,
            program_argv,
            timeout,
            capture_stderr,
            quiet_stdout,
            env,
        ),
        crate::backend::mir::JitCode::Objects(parts) => run_files_via_jit_subprocess_with(
            &parts,
            program_argv,
            timeout,
            capture_stderr,
            quiet_stdout,
            env,
        ),
    }
}

/// Run the handoff `files` (IR, or objects) through the runner. The files
/// are removed whatever happens.
#[cfg(feature = "llvm")]
fn run_files_via_jit_subprocess_with(
    files: &[std::path::PathBuf],
    program_argv: &[String],
    timeout: Option<std::time::Duration>,
    capture_stderr: bool,
    quiet_stdout: bool,
    env: &[(&str, &str)],
) -> JitRun {
    let failed = |code| JitRun {
        code,
        stderr: None,
        timed_out: false,
    };
    let remove = || {
        for f in files {
            let _ = std::fs::remove_file(f);
        }
    };
    let runner = match crate::backend::runner::locate_karac_jit_runner() {
        Some(p) => p,
        None => {
            eprintln!(
                "error: karac_jit_runner not found — set KARAC_JIT_RUNNER, or install \
                 karac with --features llvm (the runner ships beside the karac binary)"
            );
            remove();
            return failed(1);
        }
    };
    // `.status()` inherits stdin/stdout/stderr, so the JIT'd program writes
    // straight to the user's terminal and its exit code is the run's exit code.
    // Hand the PROGRAM's argv to the runner's runtime. Under the JIT the
    // hosting process is `karac_jit_runner`, so `std::env::args()` inside
    // `karac_runtime_env_args_into` reported the runner's path plus the temp
    // `.ll` file rather than anything the user wrote (B-2026-07-29-18). The
    // runtime prefers this variable when present and falls back to process
    // argv otherwise, which keeps an AOT binary — where the process IS the
    // program — on exactly the path it always used.
    //
    // Unit separator (U+001F) rather than NUL: an environment variable's value
    // is a NUL-terminated C string, so NUL cannot appear inside it. U+001F is
    // valid in a value and does not occur in real arguments.
    // `KARAC_DBG_OUTPUT` (the JIT'd program's `dbg` output format) is INHERITED
    // from this process rather than set here.
    //
    // B-2026-09-05-24: spawn + wait rather than `.status()`, so the signals
    // that would otherwise kill this wrapper and strand the runner can be
    // forwarded to it. Armed BEFORE the spawn — see `jit_child_signals`.
    // `KARAC_JIT_IR_UNLINK` hands the temp file's removal to the child, which
    // is the only party still alive on the paths where this process is killed
    // outright.
    let forwarder = jit_child_signals::Forwarder::arm();
    let mut command = std::process::Command::new(&runner);
    if capture_stderr {
        command.stderr(std::process::Stdio::piped());
    }
    if quiet_stdout {
        command.stdout(std::process::Stdio::null());
    }
    command.envs(env.iter().copied());
    let child = command
        .args(files)
        .env("KARAC_PROGRAM_ARGS", program_argv.join("\u{1F}"))
        .env("KARAC_JIT_IR_UNLINK", "1")
        // Our OWN pid, so the runner's watchdog watches the process that
        // actually spawned it rather than whatever `getppid()` happens to
        // report by the time it looks (B-2026-09-09-1).
        .env(
            crate::backend::runner::SPAWNER_PID_ENV,
            std::process::id().to_string(),
        )
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: could not spawn karac_jit_runner: {e}");
            remove();
            return failed(1);
        }
    };
    forwarder.attach(child.id());
    // Read the captured stderr on a thread of its own, so a program that
    // writes more than a pipe holds cannot block on a reader that waits.
    let reader = child.stderr.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = std::io::Read::read_to_string(&mut pipe, &mut text);
            text
        })
    });
    let mut timed_out = false;
    let status = match timeout {
        None => child.wait(),
        Some(d) => {
            let deadline = std::time::Instant::now() + d;
            loop {
                match child.try_wait() {
                    Ok(Some(s)) => break Ok(s),
                    Ok(None) if std::time::Instant::now() >= deadline => {
                        timed_out = true;
                        let _ = child.kill();
                        break child.wait();
                    }
                    Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
                    Err(e) => break Err(e),
                }
            }
        }
    };
    drop(forwarder);
    remove();
    let stderr = reader.map(|r| r.join().unwrap_or_default());
    let code = match status {
        Ok(s) => exit_code_of(&s),
        Err(e) => {
            eprintln!("error: could not wait for karac_jit_runner: {e}");
            1
        }
    };
    JitRun {
        code,
        stderr,
        timed_out,
    }
}

/// The runner's exit status as a shell-visible code, INCLUDING a death by
/// signal — B-2026-08-19-7.
///
/// `ExitStatus::code()` returns `None` on Unix when the child was killed by a
/// signal, and the old `unwrap_or(1)` collapsed every such death to a generic
/// failure. That matters here because a signal death is the CORRECT outcome for
/// the most common case: `karac run prog | head -2` closes the reader, the
/// kernel kills the runner with SIGPIPE, and the AOT binary for the same source
/// reports 141. Reporting 1 instead would keep `karac run` diverging from
/// `karac build` on the very status the sibling fix exists to align.
///
/// `128 + signal` is the shell's own convention (bash, dash, zsh) for
/// reporting a signal death through `$?`, so this is the encoding any harness
/// checking the producer's status already expects.
///
/// Gated on `llvm` because its only caller is the JIT path: without that
/// feature there is no runner to spawn, the function is dead, and CI runs the
/// DEFAULT clippy leg — which is where an ungated version fails.
#[cfg(feature = "llvm")]
fn exit_code_of(status: &std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    1
}

/// What every `karac run` does before it reads the program, on either
/// pipeline: the `--manifest`/`--no-manifest` exclusion, the discovered
/// manifest (its profile and lints apply to the run), the toolchain pin and
/// the dependency graph's diagnostics. Exits on a failure.
pub(super) fn run_manifest_preamble(
    filename: &str,
    output: OutputMode,
    manifest_override: Option<&str>,
    no_manifest: bool,
) -> Option<manifest::Manifest> {
    // Mutual exclusion at the entry point — both flags together would
    // be ambiguous (which wins?). Reject early so the operator gets a
    // clear diagnostic rather than a silent precedence rule.
    if manifest_override.is_some() && no_manifest {
        eprintln!("error: --manifest and --no-manifest are mutually exclusive");
        process::exit(1);
    }

    // Script-dir manifest discovery (tracker line 898). Unless
    // `--no-manifest` is set, walk upward from the script's own
    // directory looking for `kara.toml`. The discovered manifest's
    // `[package].profile` becomes the pipeline's active profile so
    // running a script that lives inside an embedded/kernel project
    // honors the project's compile profile. A `karac-toolchain.toml`
    // pin in the same ancestor chain is enforced here too.
    let script_dir = std::path::Path::new(filename)
        .parent()
        .map(|p| {
            if p.as_os_str().is_empty() {
                std::path::PathBuf::from(".")
            } else {
                p.to_path_buf()
            }
        })
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let discovered_manifest: Option<manifest::Manifest> = if no_manifest {
        None
    } else if let Some(explicit) = manifest_override {
        let path = std::path::PathBuf::from(explicit);
        match std::fs::read_to_string(&path) {
            Ok(src) => match manifest::parse_manifest(&path, &src) {
                Ok(m) => Some(m),
                Err(e) => {
                    emit_manifest_error(&e, output);
                    process::exit(1);
                }
            },
            Err(e) => {
                eprintln!(
                    "error: cannot read `{}` for --manifest override: {}",
                    path.display(),
                    e
                );
                process::exit(1);
            }
        }
    } else {
        // Walk upward from the script's directory. Treat a missing
        // manifest as "stdlib-only" — single-file scripts often run
        // outside any project, and the pre-line-898 behavior was to
        // not consult a manifest at all.
        match manifest::discover_project_root(&script_dir) {
            Some(root) => match manifest::load_from_root(&root) {
                Ok(m) => Some(m),
                Err(e) => {
                    emit_manifest_error(&e, output);
                    process::exit(1);
                }
            },
            None => None,
        }
    };

    if let Some(mf) = &discovered_manifest {
        super::build_cmds::emit_manifest_warnings(mf, output);
    }

    // Toolchain pin enforcement (tracker line 892) runs from the
    // script-dir ancestor chain. Skipped when --no-manifest is set
    // (the operator explicitly opted out of project-level gating).
    if !no_manifest && !enforce_toolchain_pin(&script_dir, output) {
        process::exit(1);
    }

    // Resolver follow-up (m), run slice: surface dependency-resolution
    // diagnostics before executing, so a broken dep graph (cycle / version
    // conflict / MSRV / missing path-dep / workspace-deref) fails `karac run`
    // exactly as it fails `check` / `build` — instead of the lenient path
    // silently swallowing the resolver's finding (via `quiet_dep_package_walks`)
    // and running anyway. Path-dep-only (no network), same policy as `check`:
    // registry/git deps stay lenient (unsupported findings skipped). Scoped to
    // the normal project-discovery case — `--no-manifest` opts out (as does
    // `karac run --example`, which passes it), and a `--manifest` override is a
    // single-file-script mode where project dep resolution doesn't apply.
    if !no_manifest && manifest_override.is_none() {
        if let (Some(root), Some(mf)) = (
            manifest::discover_project_root(&script_dir),
            discovered_manifest.as_ref(),
        ) {
            let mf = manifest::merge_target_overlay(mf, Some(&default_resolution_target(mf)));
            let has_deps = !mf.dependencies.is_empty()
                || !mf.dev_dependencies.is_empty()
                || mf.kara_version.is_some();
            if has_deps && !surface_dep_graph_diagnostics(&root, mf, output) {
                process::exit(1);
            }
        }
    }

    discovered_manifest
}

/// `karac check` with no file argument: check the whole package in the current
/// directory, the analysis twin of bare `karac build` (B-2026-08-20-16).
///
/// The command used to answer `error: missing file argument`, which left a user
/// who had just been told a single-file check is unreliable on a package member
/// with nowhere to go. `--concurrency-report` / `--simd-report` are accepted so
/// the flag surface matches `karac check <file>`, and noted as not yet wired for
/// project mode rather than silently ignored.
pub(super) fn cmd_check_project(
    output: OutputMode,
    concurrency_report: bool,
    simd_report: bool,
    lint_overrides: crate::lints::CliLintOverrides,
    target: Option<&str>,
    targets: Option<&[String]>,
    platforms: Option<&[crate::walker::Platform]>,
) {
    // `--target=<v1 name>`, the singular spelling, means here what it means to
    // `karac build`: check for THIS target. It has to be applied before the
    // walk, because the walk's platform-suffix filter is derived from it
    // (`--target=wasm_*` selects `_wasm` files) — design.md § Platform-specific
    // modules > Target selection, B-2026-08-20-25.
    let build_target = resolve_build_target(target);
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            process::exit(1);
        }
    };
    let (root, mf) = match manifest::load_from_cwd(&cwd) {
        Ok(ok) => ok,
        Err(e) => {
            emit_manifest_error(&e, OutputMode::Text);
            process::exit(1);
        }
    };
    if concurrency_report || simd_report {
        eprintln!(
            "note: --concurrency-report / --simd-report read a single file's late-phase \
             analysis; project-mode reporting is a follow-up. Point them at one file, or \
             use `karac build --concurrency-report`."
        );
    }
    super::build_cmds::emit_manifest_warnings(&mf, output);
    // The package's dependencies resolve as `karac build` resolves them, so
    // a cycle among them is E_DEPENDENCY_CYCLE here too rather than an
    // "unknown module" at the import (B-2026-11-13-41).
    {
        let mf = manifest::merge_target_overlay(&mf, Some(&default_resolution_target(&mf)));
        let has_deps = !mf.dependencies.is_empty()
            || !mf.dev_dependencies.is_empty()
            || mf.kara_version.is_some();
        if has_deps && !surface_dep_graph_diagnostics(&root, mf, output) {
            process::exit(1);
        }
    }
    let mut lints = lint_overrides;
    lints.apply_manifest_lints(&mf.lints);

    // `--platform=` names design.md's third axis directly instead of taking
    // what the compilation target derives — which is the host for everything
    // but `wasm_*`, and is therefore the reason the other half of a platform
    // split cannot be checked from one machine (B-2026-08-20-29). More than
    // one platform is a SWEEP: each is checked in full, under its own header,
    // and any failure fails the command.
    if let Some(platforms) = platforms {
        if platforms.len() > 1 {
            check_platform_sweep(&root, &lints, output, platforms);
            return;
        }
    }

    // The compilation-target axis, swept the same way. `--targets=` names it
    // explicitly; absent that, a package's declared `[build].targets` does —
    // which project mode used to parse and then quietly check once under the
    // default target (B-2026-08-20-29).
    let declared: Vec<String> = mf.build_targets.clone();
    let target_sweep: Option<&[String]> = match (targets, target.is_some()) {
        (Some(list), _)
            if list.len() > 1
                || list
                    .iter()
                    .any(|t| t == crate::target::ALL_TARGETS_SENTINEL) =>
        {
            Some(list)
        }
        // An explicit singular `--target=` is a request for ONE target and
        // outranks the manifest's list.
        (None, false) if declared.len() > 1 => Some(&declared),
        _ => None,
    };
    if let Some(list) = target_sweep {
        check_target_sweep(&root, &mf, &lints, output, list, platforms);
        return;
    }

    let platform = platforms
        .and_then(|p| p.first().copied())
        .unwrap_or_else(|| super::build_cmds::walk_platform_for_target(build_target));
    let mut pc = match super::build_cmds::package_check_on(&root, &lints, platform) {
        Ok(pc) => pc,
        Err(e) => {
            emit_build_error(&e, output);
            process::exit(1);
        }
    };
    if let Some(entry) = pc.lone_entry(&root) {
        // The test files first: the lone file's check ends with its verdict.
        let mut json = Vec::new();
        let test_errors = super::test_v2::check_package_tests(&root, &lints, output, &mut json);
        if test_errors > 0 {
            match output {
                OutputMode::Text => println!("\n{test_errors} error(s) found."),
                OutputMode::Json => println!("{{\"diagnostics\":[{}]}}", json.join(",")),
                OutputMode::Jsonl => {}
            }
            process::exit(1);
        }
        cmd_check(
            &entry.display().to_string(),
            output,
            None,
            None,
            platforms.map(<[_]>::to_vec),
            concurrency_report,
            simd_report,
            lints,
        );
        return;
    }
    // `run` and `build` check the package's combined source by the v2 rules
    // after this check, so `check` does too, and reports what they would
    // (B-2026-10-25-5). A type or late-phase refusal here is the v2 check's
    // to report first, as in `run_v2::combine`; a parse, cycle or resolve
    // one is the package's alone, and leaves nothing to combine.
    let mut v2_warnings_json: Vec<String> = Vec::new();
    let mut v2_effects: Option<(String, String)> = None;
    if pc.refuses_only_by_type() {
        let entry = &pc.tree.modules[pc.tree.root as usize].file;
        let filename = super::project_v2::shown(entry);
        match super::project_v2::source_files(&pc.tree, false).and_then(|files| {
            super::project_v2::amalgamate(&pc.tree, &files, |_, _| Vec::new(), "")
        }) {
            Ok(a) => {
                let passed = super::run_v2::check_v2_or_exit(
                    &filename,
                    &a.text,
                    Some(&a),
                    Some(&mf),
                    lints.clone(),
                    output,
                );
                pc.past_the_v2_check();
                // The combined source's warnings are the ones `run` and
                // `build` print, a use of another module's deprecated item
                // among them, which a module's own check cannot see
                // (B-2026-11-05-16). They stand in for the modules' own.
                match output {
                    OutputMode::Text => super::run_v2::print_diagnostics(&passed, Some(&a)),
                    OutputMode::Json | OutputMode::Jsonl => {
                        v2_warnings_json.extend(
                            collect_diagnostics(&passed)
                                .entries
                                .iter()
                                .map(|d| a.remap_json_text(&filename, d)),
                        );
                        v2_effects = Some(effects_fields(&passed));
                    }
                }
                pc.type_warnings.clear();
            }
            // A package `run` and `build` cannot combine, they refuse, and
            // so does `check`.
            Err(e) if !pc.has_errors() => super::run_v2::refused_project(&e, output, "check"),
            Err(_) => {}
        }
    }
    let mut test_json = Vec::new();
    // A package whose own files do not check has nothing for its tests to
    // build on.
    let test_errors = match pc.error_count() {
        0 => super::test_v2::check_package_tests(&root, &lints, output, &mut test_json),
        _ => 0,
    };
    let errors = pc.error_count() + test_errors;
    match output {
        // The batch document and the event stream, ids and effects included
        // (design.md § 17, B-2026-11-05-26).
        OutputMode::Json | OutputMode::Jsonl => {
            let mut diags: Vec<String> = Vec::new();
            diags.extend(super::build_cmds::parse_errors_json(&pc.parse_errors));
            diags.extend(super::build_cmds::cycles_json(&pc.cycles, &pc.tree));
            diags.extend(super::build_cmds::resolve_errors_json(&pc.resolve_errors));
            diags.extend(v2_warnings_json);
            diags.extend(super::build_cmds::type_warnings_json(&pc.type_warnings));
            diags.extend(super::build_cmds::type_errors_json(&pc.type_errors));
            if let Some(f) = &pc.late_failure {
                diags.extend(super::build_cmds::late_failure_json(f));
            }
            diags.extend(test_json);
            emit_found_entries(
                &diags,
                &package_files(&pc.tree),
                output,
                v2_effects.as_ref(),
            );
        }
        OutputMode::Text => {
            super::build_cmds::print_type_warnings_text(&pc.type_warnings);
            super::build_cmds::print_parse_errors_text(&pc.parse_errors);
            super::build_cmds::print_cycles_text(&pc.cycles, &pc.tree);
            super::build_cmds::print_resolve_errors_text(&pc.resolve_errors);
            super::build_cmds::print_type_errors_text(&pc.type_errors);
            if let Some(f) = &pc.late_failure {
                super::build_cmds::print_late_failure_text(f);
            }
            if errors == 0 {
                println!("All checks passed.");
            } else {
                println!("\n{errors} error(s) found.");
            }
        }
    }
    if errors > 0 {
        process::exit(1);
    }
}

/// `karac check --platform=all` (or any multi-platform list): check the package
/// once per platform, each under its own header, and fail if any platform does.
///
/// This is the coverage guarantee design.md's *Missing-platform rule* used to
/// promise and no command provided — the text was corrected when that was
/// measured (B-2026-08-20-25) and this is the feature (B-2026-08-20-29). Before
/// it, a `_macos` module could not be type-checked from a Linux box at all, so a
/// break in it stayed invisible until a CI matrix reached a mac.
///
/// Diagnostics are NOT deduplicated across platforms, unlike the per-file target
/// matrix. Platforms share every file that carries no suffix, so an error in one
/// of those does repeat — but a platform sweep is asked in order to learn WHICH
/// platform is broken, and a per-platform block answers that directly. The
/// target matrix dedupes because its runs differ only in a handful of
/// `#[target]`-gated items; the platform runs differ by whole files.
fn check_platform_sweep(
    root: &std::path::Path,
    lints: &crate::lints::CliLintOverrides,
    output: OutputMode,
    platforms: &[crate::walker::Platform],
) {
    let mut failed: Vec<&'static str> = Vec::new();
    for (i, platform) in platforms.iter().enumerate() {
        let name = platform.as_suffix();
        match output {
            OutputMode::Jsonl => emit_jsonl_event(
                "platform_start",
                &format!("\"platform\":{}", json_string(name)),
            ),
            OutputMode::Json => {}
            OutputMode::Text => {
                if i > 0 {
                    println!();
                }
                println!("── platform: {name} ──");
            }
        }
        let pc = match super::build_cmds::package_check_on(root, lints, *platform) {
            Ok(pc) => pc,
            Err(e) => {
                // A walk/tree failure is per-platform too: a package whose only
                // entry file carries a suffix has no entry on other platforms.
                emit_build_error(&e, output);
                failed.push(name);
                continue;
            }
        };
        let errors = pc.error_count();
        render_package_check(&pc, output, errors, Some(name));
        if errors > 0 {
            failed.push(name);
        }
        if let OutputMode::Jsonl = output {
            emit_jsonl_event(
                "platform_complete",
                &format!(
                    "\"platform\":{},\"success\":{},\"total_errors\":{}",
                    json_string(name),
                    errors == 0,
                    errors,
                ),
            );
        }
    }
    if !failed.is_empty() {
        if let OutputMode::Text = output {
            println!(
                "\n{} of {} platform(s) failed: {}",
                failed.len(),
                platforms.len(),
                failed.join(", ")
            );
        }
        process::exit(1);
    }
    if let OutputMode::Text = output {
        println!("\nAll {} platform(s) checked clean.", platforms.len());
    }
}

/// `karac check --targets=<a,b>` in project mode, or a package whose manifest
/// declares more than one `[build].targets` entry: check the package once per
/// v1 compilation target.
///
/// The platform axis rides along — each target derives its own OS platform
/// unless `--platform=` named one — so `--targets=native,wasm_wasi` checks the
/// host's files under `native` and the `_wasm` files under `wasm_wasi`, which
/// is the pair a package with a platform split most wants verified.
fn check_target_sweep(
    root: &std::path::Path,
    mf: &manifest::Manifest,
    lints: &crate::lints::CliLintOverrides,
    output: OutputMode,
    targets: &[String],
    platforms: Option<&[crate::walker::Platform]>,
) {
    // `--targets=all` over a package: the v1 set, and every target an item
    // in the package's sources is gated on, so its body is checked where it
    // is written for.
    let all: Vec<String>;
    let targets = if targets
        .iter()
        .any(|t| t == crate::target::ALL_TARGETS_SENTINEL)
    {
        let mut list: Vec<String> = crate::target::ALL_CHECK_TARGETS
            .iter()
            .map(|t| t.to_string())
            .collect();
        for t in package_gated_check_targets(&root.join("src")) {
            if !list.iter().any(|a| a == t) {
                list.push(t.to_string());
            }
        }
        all = list;
        &all[..]
    } else {
        targets
    };
    let mut failed: Vec<String> = Vec::new();
    // Each target's check, run first and reported after, so a finding every
    // target makes is reported once under "all targets" (design.md § 16
    // Checking several targets: Deduplication), as for a file.
    let mut runs: Vec<(String, TargetCheck)> = Vec::new();
    for target in targets {
        if let Err(e) = crate::target::set_active_target(target) {
            eprintln!("error: {e}");
            process::exit(1);
        }
        let platform = platforms
            .and_then(|p| p.first().copied())
            .unwrap_or_else(|| super::build_cmds::walk_platform_for_target(target));
        let run = check_package_for_target(root, mf, lints, platform);
        if run.errors() > 0 {
            failed.push(target.clone());
        }
        runs.push((target.clone(), run));
    }
    // The v2 check's findings that every target made.
    let shared: Vec<String> = match runs.split_first() {
        Some(((_, TargetCheck::V2 { blocks, .. }), rest)) if !rest.is_empty() => blocks
            .iter()
            .filter(|b| {
                rest.iter().all(|(_, r)| match r {
                    TargetCheck::V2 { blocks, .. } => blocks.contains(*b),
                    _ => false,
                })
            })
            .cloned()
            .collect(),
        _ => Vec::new(),
    };
    match output {
        // JSON is one object for the whole sweep, as for a file: each
        // target's own findings under `targets`, those on every target once
        // under `shared_diagnostics`.
        OutputMode::Json => {
            let json_runs: Vec<(String, usize, Vec<String>)> = runs
                .iter()
                .map(|(t, r)| (t.clone(), r.errors(), r.json_entries()))
                .collect();
            let per_target: Vec<(&str, usize, &[String])> = json_runs
                .iter()
                .map(|(t, n, e)| (t.as_str(), *n, &e[..]))
                .collect();
            let (blocks, shared) = split_target_json(&per_target);
            print_stdout_line(&format!(
                "{{\"targets\":[{blocks}],\"shared_diagnostics\":[{shared}],\"success\":{}}}",
                failed.is_empty()
            ));
        }
        OutputMode::Jsonl => {
            for (target, run) in &runs {
                emit_jsonl_event(
                    "target_start",
                    &format!("\"target\":{}", json_string(target)),
                );
                match run {
                    TargetCheck::Package(pc, errors) => {
                        render_package_check(pc, output, *errors, None)
                    }
                    TargetCheck::V2 { json, .. } => {
                        for e in json {
                            println!("{e}");
                        }
                    }
                    TargetCheck::Refused(e) => emit_build_error(e, output),
                }
                emit_jsonl_event(
                    "target_complete",
                    &format!(
                        "\"target\":{},\"success\":{},\"total_errors\":{}",
                        json_string(target),
                        run.errors() == 0,
                        run.errors(),
                    ),
                );
            }
        }
        OutputMode::Text => {
            if !shared.is_empty() {
                println!("── all targets ──");
                for block in &shared {
                    eprintln!("{block}");
                }
            }
            for (i, (target, run)) in runs.iter().enumerate() {
                if i > 0 || !shared.is_empty() {
                    println!();
                }
                println!("── target: {target} ──");
                match run {
                    TargetCheck::Package(pc, errors) => {
                        render_package_check(pc, output, *errors, None)
                    }
                    TargetCheck::V2 { blocks, errors, .. } => {
                        let own: Vec<&String> =
                            blocks.iter().filter(|b| !shared.contains(*b)).collect();
                        for block in &own {
                            eprintln!("{block}");
                        }
                        // As a file's target sweep words it.
                        println!("{errors} error(s) under target '{target}'.");
                    }
                    TargetCheck::Refused(e) => emit_build_error(e, output),
                }
            }
        }
    }
    if !failed.is_empty() {
        if let OutputMode::Text = output {
            println!(
                "\n{} of {} target(s) failed: {}",
                failed.len(),
                targets.len(),
                failed.join(", ")
            );
        }
        process::exit(1);
    }
    if let OutputMode::Text = output {
        println!("\nAll {} target(s) checked clean.", targets.len());
    }
}

/// One target's check of a package in a target sweep.
enum TargetCheck {
    /// The package check's own verdict: a parse, cycle or resolve refusal,
    /// or what is left of a type refusal once the v2 check passed (its
    /// module-boundary rules).
    Package(Box<super::build_cmds::PackageCheck>, usize),
    /// The v2 check of the combined source refused it: its findings, naming
    /// the package's files, as text blocks and JSON entries.
    V2 {
        blocks: Vec<String>,
        json: Vec<String>,
        errors: usize,
    },
    /// The package could not be walked or combined.
    Refused(String),
}

impl TargetCheck {
    fn errors(&self) -> usize {
        match self {
            TargetCheck::Package(_, errors) => *errors,
            TargetCheck::V2 { errors, .. } => *errors,
            TargetCheck::Refused(_) => 1,
        }
    }

    fn json_entries(&self) -> Vec<String> {
        match self {
            TargetCheck::Package(pc, _) => package_check_json_entries(pc),
            TargetCheck::V2 { json, .. } => json.clone(),
            TargetCheck::Refused(e) => vec![format!(
                "{{\"severity\":\"error\",\"phase\":\"build\",\"concept\":\"compilation/pipeline\",\"message\":{}}}",
                json_string(e)
            )],
        }
    }
}

/// What `karac check` reports for a package under the active target: the
/// v2 check of its combined source decides a type refusal, as `run` and
/// `build` do and as a one-target check does (`cmd_check_project`), so a
/// target sweep refuses nothing a one-target check passes (B-2026-11-08-38:
/// the per-module typecheck refused `s.split(" ").count()`, whose library
/// signature only the v2 check reads).
fn check_package_for_target(
    root: &std::path::Path,
    mf: &manifest::Manifest,
    lints: &crate::lints::CliLintOverrides,
    platform: crate::walker::Platform,
) -> TargetCheck {
    let mut pc = match super::build_cmds::package_check_on(root, lints, platform) {
        Ok(pc) => pc,
        Err(e) => return TargetCheck::Refused(e),
    };
    if pc.refuses_only_by_type() {
        let entry = &pc.tree.modules[pc.tree.root].file;
        let filename = super::project_v2::shown(entry);
        let combined = super::project_v2::source_files(&pc.tree, false).and_then(|files| {
            super::project_v2::amalgamate(&pc.tree, &files, |_, _| Vec::new(), "")
        });
        match combined {
            Ok(a) => {
                let mut pipeline =
                    super::run_v2::v2_pipeline(&filename, &a.text, Some(mf), lints.clone());
                pipeline.run_all_checks();
                let errors = pipeline.total_errors();
                if errors > 0 {
                    return TargetCheck::V2 {
                        blocks: render_text_diagnostics(&pipeline)
                            .iter()
                            .map(|b| a.remap_text(&filename, b))
                            .collect(),
                        json: collect_diagnostics(&pipeline)
                            .entries
                            .iter()
                            .map(|d| a.remap_json_text(&filename, d))
                            .collect(),
                        errors,
                    };
                }
                pc.past_the_v2_check();
                pc.type_warnings.clear();
            }
            Err(e) if !pc.has_errors() => return TargetCheck::Refused(e.to_string()),
            Err(_) => {}
        }
    }
    let errors = pc.error_count();
    TargetCheck::Package(Box::new(pc), errors)
}

/// Render one [`PackageCheck`] in the requested output mode. Split out of
/// `cmd_check_project` so the platform sweep renders each run identically to a
/// single check — `platform` is `Some` only inside a sweep, where the text mode
/// has already printed the header and must not repeat the summary line.
/// A package check's diagnostics as JSON entries, in render order.
fn package_check_json_entries(pc: &super::build_cmds::PackageCheck) -> Vec<String> {
    let mut diags: Vec<String> = Vec::new();
    diags.extend(super::build_cmds::parse_errors_json(&pc.parse_errors));
    diags.extend(super::build_cmds::type_warnings_json(&pc.type_warnings));
    diags.extend(super::build_cmds::cycles_json(&pc.cycles, &pc.tree));
    diags.extend(super::build_cmds::resolve_errors_json(&pc.resolve_errors));
    diags.extend(super::build_cmds::type_errors_json(&pc.type_errors));
    diags
}

fn render_package_check(
    pc: &super::build_cmds::PackageCheck,
    output: OutputMode,
    errors: usize,
    platform: Option<&str>,
) {
    match output {
        OutputMode::Json => {
            let diags = package_check_json_entries(pc);
            match platform {
                Some(name) => println!(
                    "{{\"platform\":{},\"diagnostics\":[{}]}}",
                    json_string(name),
                    diags.join(",")
                ),
                None => println!("{{\"diagnostics\":[{}]}}", diags.join(",")),
            }
        }
        OutputMode::Jsonl => {
            for e in super::build_cmds::parse_errors_jsonl(&pc.parse_errors) {
                println!("{e}");
            }
            for e in super::build_cmds::cycles_jsonl(&pc.cycles, &pc.tree) {
                println!("{e}");
            }
            for e in super::build_cmds::resolve_errors_jsonl(&pc.resolve_errors) {
                println!("{e}");
            }
            for e in super::build_cmds::type_errors_jsonl(&pc.type_errors) {
                println!("{e}");
            }
        }
        OutputMode::Text => {
            super::build_cmds::print_type_warnings_text(&pc.type_warnings);
            super::build_cmds::print_parse_errors_text(&pc.parse_errors);
            super::build_cmds::print_cycles_text(&pc.cycles, &pc.tree);
            super::build_cmds::print_resolve_errors_text(&pc.resolve_errors);
            super::build_cmds::print_type_errors_text(&pc.type_errors);
            if errors == 0 {
                println!("All checks passed.");
            } else {
                println!("\n{errors} error(s) found.");
            }
        }
    }
}

/// `karac check <file>` where `<file>` is a member of a `kara.toml` package.
///
/// Runs the WHOLE package through the module-aware pipeline (the same one
/// `karac build` runs) and then renders only the requested file's diagnostics,
/// with a one-line pointer when sibling modules carry errors of their own.
///
/// The alternative — refusing the invocation the way single-file `karac build`
/// does — was rejected: `karac check <file>` is what editors, the Mend loop and
/// every `karac fix` cycle call, and answering "run it differently" to all of
/// them trades an incorrect answer for no answer. Widening the ANALYSIS while
/// keeping the REPORT scoped to the file the caller named gives the correct
/// answer to the question actually asked.
///
/// With `script`, `<file>` is a script inside the package but outside its
/// `src/`, and the program checked is the script beside the package's
/// dependencies (B-2026-11-13-30).
fn cmd_check_package_member(
    filename: &str,
    root: &std::path::Path,
    script: Option<&std::path::Path>,
    output: OutputMode,
    lint_overrides: &crate::lints::CliLintOverrides,
    platform: Option<crate::walker::Platform>,
) -> bool {
    let mut lints = lint_overrides.clone();
    if let Ok(mf) = manifest::load_from_root(root) {
        lints.apply_manifest_lints(&mf.lints);
    }
    // `--platform=` overrides what the compilation target derives, so a
    // `_macos` member can be checked from any host (B-2026-08-20-29).
    let platform = platform.unwrap_or_else(|| {
        super::build_cmds::walk_platform_for_target(crate::target::active_target())
    });
    // The machine-readable reports are the ones `karac build` checks with:
    // the package as one source through every phase, its places named by
    // file, ids and the event stream included (design.md § 17,
    // B-2026-11-05-26). The text report stays scoped to the named file.
    if output != OutputMode::Text && platform == crate::walker::Platform::host() {
        if let Some((tree_root, tree)) = super::project_v2::package_tree(filename, false, false) {
            if tree.modules.iter().filter(|m| !m.is_synthetic).count() > 1 {
                check_package_machine(filename, &tree_root, &tree, &lints, output);
            }
        }
    }
    let checked = match script {
        Some(script) => super::build_cmds::script_check(root, script, &lints),
        None => super::build_cmds::package_check_on(root, &lints, platform),
    };
    let mut pc = match checked {
        Ok(pc) => pc,
        Err(e) => {
            // No package VIEW could be formed at all — most often a directory
            // holding a `kara.toml` and modules but no `src/main.kara` /
            // `src/lib.kara` entry. Several `examples/` packages are shaped
            // that way, and `karac build` refuses them for the same reason.
            //
            // Fall back to the single-file check rather than failing: the
            // caller asked about one file, and "this package has no entry
            // point" is not an answer about that file. The single-file view is
            // the blind one this function exists to replace, so say so once —
            // narrowing silently would be the original bug wearing a new hat.
            if output == OutputMode::Text {
                eprintln!(
                    "note: checking `{filename}` on its own — the package at `{}` could \
                     not be assembled ({e}), so imports of sibling modules are not \
                     resolved here",
                    root.display(),
                );
            }
            return false;
        }
    };
    // A lone `src/main.kara` is built as a file, so it is checked as one
    // (B-2026-10-24-39).
    if pc.lone_entry(root).is_some() {
        return false;
    }
    let had_errors = pc.has_errors();
    // Machine output that reaches here is a package the combined source could
    // not be formed for (a parse error in a module, a cycle): the package
    // check's findings, all of them, each naming its file.
    if output != OutputMode::Text {
        let mut diags: Vec<String> = Vec::new();
        diags.extend(super::build_cmds::parse_errors_json(&pc.parse_errors));
        diags.extend(super::build_cmds::cycles_json(&pc.cycles, &pc.tree));
        diags.extend(super::build_cmds::resolve_errors_json(&pc.resolve_errors));
        diags.extend(super::build_cmds::type_warnings_json(&pc.type_warnings));
        diags.extend(super::build_cmds::type_errors_json(&pc.type_errors));
        emit_found_entries(&diags, &package_files(&pc.tree), output, None);
        process::exit(i32::from(had_errors));
    }
    let elsewhere = pc.restrict_to_file(std::path::Path::new(filename));

    match output {
        OutputMode::Json | OutputMode::Jsonl => unreachable!("reported above"),
        OutputMode::Text => {
            super::build_cmds::print_type_warnings_text(&pc.type_warnings);
            super::build_cmds::print_parse_errors_text(&pc.parse_errors);
            super::build_cmds::print_cycles_text(&pc.cycles, &pc.tree);
            super::build_cmds::print_resolve_errors_text(&pc.resolve_errors);
            super::build_cmds::print_type_errors_text(&pc.type_errors);
            let shown = pc.error_count();
            if shown == 0 && elsewhere == 0 {
                println!("All checks passed.");
            } else if shown > 0 {
                println!("\n{shown} error(s) found.");
            }
            // Never let a sibling module's real error vanish just because the
            // caller named one file: say it exists and where to see it.
            if elsewhere > 0 {
                eprintln!(
                    "note: {elsewhere} further error(s) in other modules of the package at \
                     `{}` — run `cd {} && karac build` to see them",
                    root.display(),
                    root.display(),
                );
            }
        }
    }
    if had_errors {
        process::exit(1);
    }
    true
}

/// [`cmd_check_package_member`]'s `--output=json` / `jsonl`: the package's
/// combined source checked as `karac build` checks it, then reported in its
/// files. A refusal only the module boundaries carry is the package check's,
/// reported in the same shapes. Exits: 1 on an error, 0 otherwise.
fn check_package_machine(
    filename: &str,
    root: &std::path::Path,
    tree: &crate::module::ProgramTree,
    lints: &crate::lints::CliLintOverrides,
    output: OutputMode,
) -> ! {
    let a = super::run_v2::combine(root, tree, output, "check");
    let mf = manifest::load_from_root(root).ok();
    let remap = |d: &str| a.remap_json_text(filename, d);
    let fresh = || super::run_v2::v2_pipeline(filename, &a.text, mf.as_ref(), lints.clone());
    let mut pipeline = fresh();
    pipeline.run_all_checks();
    let errors = pipeline.total_errors();
    if errors == 0 {
        if let Some(pc) = &a.refusal {
            render_package_refusal(pc, output);
        }
    }
    match output {
        OutputMode::Json => emit_json_output_remapped(&pipeline, &remap),
        _ => run_pipeline_jsonl_remapped(&mut fresh(), &a.files(), &remap),
    }
    process::exit(i32::from(errors > 0));
}

#[allow(clippy::too_many_arguments)]
pub(super) fn cmd_check(
    filename: &str,
    output: OutputMode,
    profiles: Option<Vec<crate::manifest::CompileProfile>>,
    targets: Option<Vec<String>>,
    platforms: Option<Vec<crate::walker::Platform>>,
    concurrency_report: bool,
    simd_report: bool,
    lint_overrides: crate::lints::CliLintOverrides,
) {
    // Both drivers are "run the pipeline N times and group diagnostics"
    // matrices; combining them would be an N×M product nobody has asked
    // for. Reject loudly rather than picking a silent precedence.
    if profiles.is_some() && targets.is_some() {
        eprintln!("error: --profiles and --targets are mutually exclusive");
        process::exit(1);
    }
    // A platform SWEEP reports per platform, and a file-mode check reports one
    // file — which may not even exist on every platform in the list. The sweep
    // is a project-mode question, so say so rather than inventing a per-file
    // rendering for it (B-2026-08-20-29).
    let platform = match platforms.as_deref() {
        Some([]) | None => None,
        Some([one]) => Some(*one),
        Some(many) => {
            eprintln!(
                "error: --platform with {} platforms is a package-wide sweep; drop the \
                 file argument (`karac check --platform=all`) to run it",
                many.len()
            );
            process::exit(1);
        }
    };

    // Resolver follow-up (m): when `karac check <file>` runs inside a project,
    // surface dependency-resolution diagnostics so a broken dep graph (cycle /
    // version conflict / MSRV / missing path-dep / workspace-deref) fails the
    // check exactly as it fails `karac build`. Runs once up front, before the
    // profiles/targets/single dispatch below, so it fires regardless of matrix
    // mode. No-op for a single-file script outside any project, or a project
    // that declares no deps / MSRV. Path-dep-only (no network) — see
    // `surface_dep_graph_diagnostics`.
    let file_dir = std::path::Path::new(filename)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    if let Some(root) = manifest::discover_project_root(&file_dir) {
        if let Ok(mf) = manifest::load_from_root(&root) {
            super::build_cmds::emit_manifest_warnings(&mf, output);
            let mf = manifest::merge_target_overlay(&mf, Some(&default_resolution_target(&mf)));
            let has_deps = !mf.dependencies.is_empty()
                || !mf.dev_dependencies.is_empty()
                || mf.kara_version.is_some();
            if has_deps && !surface_dep_graph_diagnostics(&root, mf, output) {
                process::exit(1);
            }
        }
    }

    let source = read_source(filename);

    if let Some(list) = profiles {
        cmd_check_profiles(filename, &source, output, &list, lint_overrides);
        return;
    }

    // Multi-target verification (phase-10): `--targets=` wins; absent,
    // consult the discovered manifest's `[build].targets` (walking
    // upward from the file's own directory, same discovery rule as
    // `karac run`). An empty/undeclared list keeps the single-pass
    // default below — check under the active (`native`) target.
    // `--targets=all`: the v1 set, and every target the source gates an
    // item on, so its body is checked where it is written for.
    let targets = targets.map(|list| {
        if list
            .iter()
            .any(|t| t == crate::target::ALL_TARGETS_SENTINEL)
        {
            let mut all: Vec<String> = crate::target::ALL_CHECK_TARGETS
                .iter()
                .map(|t| t.to_string())
                .collect();
            for t in gated_check_targets(&source) {
                if !all.iter().any(|a| a == t) {
                    all.push(t.to_string());
                }
            }
            all
        } else {
            list
        }
    });
    let targets = targets.or_else(|| {
        let declared = manifest_build_targets_for(filename, output);
        if declared.is_empty() {
            None
        } else {
            Some(declared)
        }
    });
    // Neither asked for nor declared: cover whatever the SOURCE ITSELF
    // gates. A `#[target(T)]` item is deleted before resolution under any
    // other target, so a single-target check never reads its body —
    // "All checks passed" on a fn with two plain type errors in it
    // (B-2026-08-05-29). A `#[target]` annotation names the target the
    // body is written for, so checking it there is what the annotation
    // asks for; files with no gated items keep the single-pass default
    // and pay nothing.
    let targets = targets.or_else(|| {
        let extra = gated_check_targets(&source);
        if extra.is_empty() {
            return None;
        }
        let mut list = vec![crate::target::active_target().to_string()];
        list.extend(extra.into_iter().map(str::to_string));
        Some(list)
    });
    if let Some(list) = targets {
        cmd_check_targets(filename, &source, output, &list, lint_overrides);
        return;
    }

    // A file inside a package's `src/` is not a program on its own: its
    // `import`s name sibling modules that a single-file check never loads, so
    // the pipeline below would answer about a program the user did not write
    // and `karac build` will not build (B-2026-08-20-16). Route it through the
    // same module-aware walk → resolve → typecheck the build runs.
    //
    // Placed AFTER the `--profiles` / `--targets` dispatches on purpose: those
    // are matrices over one file's pipeline, and a package member under them
    // keeps the single-file behaviour rather than gaining a half-defined
    // product of two matrices. The single-pass path below is the one the Mend
    // loop and every plain `karac check <file>` take, and the one that was
    // reporting invented type errors.
    if let Some(root) = package_root_of_member(filename) {
        if cmd_check_package_member(filename, &root, None, output, &lint_overrides, platform) {
            return;
        }
    }
    // A script inside a package but outside its `src/` is checked as
    // `karac run` runs it: with the package's dependencies (design.md § 4,
    // Finding the package, B-2026-11-13-30).
    if let Some((root, tree)) = super::project_v2::package_tree(filename, false, false) {
        if let Some(script) = super::project_v2::script_of(&root, &tree) {
            if cmd_check_package_member(
                filename,
                &root,
                Some(&script),
                output,
                &lint_overrides,
                platform,
            ) {
                return;
            }
        }
    }

    match output {
        OutputMode::Jsonl => {
            let mut pipeline = Pipeline::new(filename, &source)
                .with_lint_overrides(lint_overrides)
                .strict_core()
                .with_library();
            run_pipeline_jsonl(&mut pipeline);
            if pipeline.total_errors() > 0 {
                process::exit(1);
            }
            std::mem::forget(pipeline);
        }
        _ => {
            let mut pipeline = Pipeline::new(filename, &source)
                .with_lint_overrides(lint_overrides)
                .strict_core()
                .with_library();
            pipeline.run_all_checks();

            // Slice D: concurrency report fires after `run_all_checks` and
            // before the final OK / error summary so the report sits with the
            // rest of stdout. Only the report needs the analysis.
            if concurrency_report {
                pipeline.concurrencycheck();
                emit_concurrency_report(&pipeline);
            }
            // SIMD lowering report (slice 5b) — same render-side placement.
            if simd_report {
                emit_simd_report(&pipeline);
            }

            match output {
                OutputMode::Text => {
                    print_text_diagnostics(&pipeline);
                    let total = pipeline.total_errors();
                    // Before the verdict, not after: "All checks passed" is
                    // exactly the line the note qualifies (B-2026-08-05-29).
                    if let Some(note) = target_skip_note(&pipeline) {
                        eprintln!("\n{note}");
                    }
                    if total > 0 {
                        eprintln!("\n{total} error(s) found.");
                        process::exit(1);
                    } else {
                        eprintln!("All checks passed.");
                    }
                }
                OutputMode::Json => {
                    emit_json_output(&pipeline);
                    if pipeline.total_errors() > 0 {
                        process::exit(1);
                    }
                }
                OutputMode::Jsonl => unreachable!(),
            }
            // The process ends here: freeing the checked program and the
            // library's AST node by node cost about 4% of a small check
            // (B-2026-11-09-28).
            std::mem::forget(pipeline);
        }
    }
}

/// Slice D helper: render the human-readable concurrency report from the
/// pipeline's already-populated `concurrency` and `effects` fields and
/// emit it to stdout. No-op when either field is None (the analysis didn't
/// run because earlier phases failed); the build/check paths still surface
/// the upstream errors through the normal diagnostic channel.
pub(super) fn emit_concurrency_report(pipeline: &Pipeline) {
    let (Some(concurrency), Some(effects)) = (&pipeline.concurrency, &pipeline.effects) else {
        return;
    };
    let report = crate::concurrency_report::render_concurrency_report(
        concurrency,
        effects,
        &pipeline.parsed.program,
    );
    print!("{report}");
}

/// `--simd-report=verbose` helper (slice 5b): render the per-function SIMD
/// lowering-tier report from the typechecked program and emit it to stdout.
/// Reuses `simd_report::analyze_program` — the same walk `simd_check` runs —
/// but renders *all* tiers (Native/Wide/Scalar), not just the `#[require_simd]`
/// errors. A no-op-shaped report (`<no vector operations>`) when the program
/// has no vector ops or typecheck didn't run.
pub(super) fn emit_simd_report(pipeline: &Pipeline) {
    let findings =
        crate::simd_report::analyze_program(&pipeline.parsed.program, pipeline.typed.as_ref());
    print!("{}", crate::simd_report::render_simd_report(&findings));
}

/// Multi-profile typecheck driver. Runs the full pipeline once per named
/// profile and groups diagnostics by profile so a CI matrix can verify
/// "this library compiles cleanly under default + embedded + kernel" from a
/// single invocation. Exits non-zero if any profile fails. Profile only
/// affects the effect-checker today (extern declarations are validated
/// against the profile's forbidden-effect set per `manifest::CompileProfile`),
/// so the parse / resolve / typecheck phases produce identical output across
/// profiles — only the effect phase diverges. Per-profile grouping keeps the
/// output skimmable when one profile fails and the others pass.
pub(super) fn cmd_check_profiles(
    filename: &str,
    source: &str,
    output: OutputMode,
    profiles: &[crate::manifest::CompileProfile],
    lint_overrides: crate::lints::CliLintOverrides,
) {
    let mut any_failed = false;
    let mut blocks: Vec<String> = Vec::new();
    for (idx, profile) in profiles.iter().enumerate() {
        let mut pipeline = Pipeline::new(filename, source)
            .with_lint_overrides(lint_overrides.clone())
            .strict_core()
            .with_library();
        pipeline.profile = *profile;

        match output {
            OutputMode::Text => {
                pipeline.run_all_checks();
                let total = pipeline.total_errors();
                if total > 0 {
                    any_failed = true;
                }
                if idx > 0 {
                    eprintln!();
                }
                eprintln!("── profile: {} ──", profile.as_str());
                print_text_diagnostics(&pipeline);
                if total > 0 {
                    eprintln!("{total} error(s) under '{}' profile.", profile.as_str());
                } else {
                    eprintln!("All checks passed under '{}' profile.", profile.as_str());
                }
            }
            OutputMode::Json => {
                pipeline.run_all_checks();
                let total = pipeline.total_errors();
                if total > 0 {
                    any_failed = true;
                }
                let diags = collect_diagnostics(&pipeline);
                let block = format!(
                    "{{\"profile\":{},\"success\":{},\"total_errors\":{},\"diagnostics\":{}}}",
                    json_string(profile.as_str()),
                    total == 0,
                    total,
                    diags.to_json_array(),
                );
                blocks.push(block);
            }
            OutputMode::Jsonl => {
                emit_jsonl_event(
                    "profile_start",
                    &format!("\"profile\":{}", json_string(profile.as_str())),
                );
                run_pipeline_jsonl(&mut pipeline);
                let total = pipeline.total_errors();
                if total > 0 {
                    any_failed = true;
                }
                emit_jsonl_event(
                    "profile_complete",
                    &format!(
                        "\"profile\":{},\"success\":{},\"total_errors\":{}",
                        json_string(profile.as_str()),
                        total == 0,
                        total,
                    ),
                );
            }
        }
    }

    if let OutputMode::Json = output {
        println!(
            "{{\"profiles\":[{}],\"success\":{}}}",
            blocks.join(","),
            !any_failed,
        );
    }

    if any_failed {
        process::exit(1);
    }
}

/// Source-side trigger for multi-target check: the targets named by
/// `#[target(...)]` attributes in `source` that a native check would not
/// reach. See `target::extra_check_targets_for` for why a gated body is
/// otherwise never examined (B-2026-08-05-29).
///
/// Parses on its own rather than reusing the pipeline's parse, because
/// the target list has to be known *before* the first pipeline is built.
/// A source that does not parse yields no extra targets and falls through
/// to the single-pass path, which reports the parse errors — discovery
/// must never be the thing that reports them.
///
/// Keyed on the ACTIVE target rather than on `native`: the non-llvm
/// `karac build --target=T` fallback reaches `cmd_check` with T already
/// active, and there the user asked for exactly T — a discovery that
/// hardcoded `native` would silently widen that one-target request into
/// a matrix.
///
/// SCOPE: the entry file's own top-level items. A `#[target]` inside an
/// IMPORTED module is not discovered here, so a gated body in a
/// dependency module still needs an explicit `--targets`. Recorded on
/// B-2026-08-05-29 rather than silently narrowed.
/// [`gated_check_targets`] over every `.kara` file under `dir`, every
/// platform half included.
fn package_gated_check_targets(dir: &std::path::Path) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut paths: Vec<std::path::PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        let found = if p.is_dir() {
            package_gated_check_targets(&p)
        } else if p.extension().is_some_and(|e| e == "kara") {
            std::fs::read_to_string(&p)
                .map(|s| gated_check_targets(&s))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        for t in found {
            if !out.contains(&t) {
                out.push(t);
            }
        }
    }
    out
}

pub(super) fn gated_check_targets(source: &str) -> Vec<&'static str> {
    let parsed = crate::parse_shared(source);
    if !parsed.errors.is_empty() {
        return Vec::new();
    }
    crate::target::extra_check_targets_for(&parsed.program.items, crate::target::active_target())
}

/// Manifest-side trigger for multi-target check: walk upward from the
/// checked file's own directory (the `karac run` discovery rule —
/// the file's filesystem location is the stable identity, not the
/// cwd) and return the discovered manifest's `[build].targets`.
/// No manifest found → empty (single-file scripts outside any project
/// keep the single-pass default). A malformed manifest is a hard error
/// — same posture as `karac run`'s discovery.
pub(super) fn manifest_build_targets_for(filename: &str, output: OutputMode) -> Vec<String> {
    let file_dir = std::path::Path::new(filename)
        .parent()
        .map(|p| {
            if p.as_os_str().is_empty() {
                std::path::PathBuf::from(".")
            } else {
                p.to_path_buf()
            }
        })
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    match manifest::discover_project_root(&file_dir) {
        Some(root) => match manifest::load_from_root(&root) {
            Ok(m) => m.build_targets,
            Err(e) => {
                emit_manifest_error(&e, output);
                process::exit(1);
            }
        },
        None => Vec::new(),
    }
}

/// Read the `KARAC_TARGET_CPU` env var — the middle tier of the
/// `--target-cpu` precedence chain (flag, then env, then `[profile.release]
/// target-cpu`, then the per-target default table). Empty /
/// whitespace-only is treated
/// as unset so `KARAC_TARGET_CPU= karac build …` can neutralize an
/// outer-scope export without tripping validation.
#[cfg(feature = "llvm")]
pub(super) fn read_target_cpu_env() -> Option<String> {
    std::env::var("KARAC_TARGET_CPU")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Read the `KARAC_TARGET_FEATURES` env var — the middle tier of the
/// `--target-features` precedence chain (resolved independently of the
/// CPU chain). Same empty-means-unset contract as `read_target_cpu_env`.
#[cfg(feature = "llvm")]
pub(super) fn read_target_features_env() -> Option<String> {
    std::env::var("KARAC_TARGET_FEATURES")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Discover the manifest by walking upward from the built file's own
/// directory (the `karac run` discovery rule, same shape as
/// `manifest_build_targets_for` above) and return one `[profile.release]` field
/// picked by `pick`. No manifest → `None`. A malformed manifest is a
/// hard error — but note the callers only reach this tier when neither
/// the CLI flag nor the env var supplied a value, so explicit overrides
/// never gain a manifest failure mode. (The cpu and features chains
/// resolve lazily and independently, so a build may walk twice — the
/// walk is cheap and idempotent.)
#[cfg(feature = "llvm")]
pub(super) fn manifest_release_field_for(
    filename: &str,
    output: OutputMode,
    pick: fn(&manifest::Manifest) -> Option<String>,
) -> Option<String> {
    let file_dir = std::path::Path::new(filename)
        .parent()
        .map(|p| {
            if p.as_os_str().is_empty() {
                std::path::PathBuf::from(".")
            } else {
                p.to_path_buf()
            }
        })
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    match manifest::discover_project_root(&file_dir) {
        Some(root) => match manifest::load_from_root(&root) {
            Ok(m) => pick(&m),
            Err(e) => {
                emit_manifest_error(&e, output);
                process::exit(1);
            }
        },
        None => None,
    }
}

/// Resolve the arch-portable `cpu-baseline` level to its concrete native
/// `(target-cpu, target-features)` override, applying the design default `v3`
/// when no explicit level is declared. Native non-wasm only — wasm gets no CPU
/// baseline (`(None, None)`). The mapped value is the LOWEST tier of the CPU /
/// feature chains, so an explicit `--target-cpu` / `KARAC_TARGET_CPU` /
/// `[profile.release] target-cpu` (or `[profile.release] cpu-baseline`) always wins.
///
/// **Deploy-baseline commitment (design.md § Multiversioning > `cpu-baseline`,
/// default `"v3"`):** a default `karac build` now targets `x86-64-v3` on x86-64
/// (Haswell+, ~2013 — excludes pre-Haswell) and `+v8.4a` on aarch64 (Apple M1+,
/// Graviton 3+). This narrows the deploy set for sharper codegen — the design's
/// single-binary-distribution posture. Pre-baseline hardware opts down with
/// `[profile.release] cpu-baseline = "v1" | "v2"` (or an explicit `--target-cpu`). The
/// codegen / asan / abi test harnesses drive `compile_to_object*` directly (not
/// this CLI path), so they stay on the per-target `generic` default and the
/// heavy CI legs are unaffected; the CLI-spawned build tests run on x86 (v3) and
/// `macos-latest` (Apple M1+, ≥ v8.4), both at or above the baseline.
#[cfg(feature = "llvm")]
pub(super) fn resolve_native_cpu_baseline(
    explicit: Option<&str>,
) -> (Option<String>, Option<String>) {
    if crate::target::active_target_is_wasm() {
        return (None, None);
    }
    let level = explicit.unwrap_or("v3");
    // A cross build's baseline is its target's architecture's.
    let arch = crate::target::cross_triple()
        .and_then(|t| t.split('-').next())
        .unwrap_or(std::env::consts::ARCH);
    crate::manifest::cpu_baseline_native_override(level, arch)
}

/// Act on the resolved `--target-cpu` value (phase-10; design.md § CPU
/// Baseline Targeting). `None` — the common case — keeps the per-target
/// default table. The literal `help` prints LLVM's supported-CPU
/// listing for the active target and exits 0 (`rustc -C
/// target-cpu=help` mirror). Any other name is validated against that
/// same listing — LLVM's native behavior on an unknown CPU is
/// warn-and-fall-back-to-generic, i.e. exactly the silent baseline
/// neutering the validation closes — then installed process-wide for
/// the codegen driver's target-machine constructors.
#[cfg(feature = "llvm")]
pub(super) fn apply_target_cpu_override(resolved: Option<String>) {
    let Some(cpu) = resolved else { return };
    if cpu == "help" {
        crate::backend::driver::print_target_cpu_help();
        process::exit(0);
    }
    if let Err(msg) = crate::backend::driver::validate_target_cpu(&cpu) {
        eprintln!("{msg}");
        process::exit(1);
    }
    crate::target::set_target_cpu_override(&cpu);
}

/// Act on the resolved `--target-features` value — the
/// `apply_target_cpu_override` sibling (design.md § CPU Baseline
/// Targeting > Feature-string override). `help` prints the same
/// per-target dump (its `Available features` section is the relevant
/// half) and exits 0. Any other value is token-validated (`+`/`-`
/// prefixes, names in LLVM's per-target feature registry — LLVM's
/// native behavior on an unknown feature is warn-and-ignore, the same
/// silent neutering the CPU validation closes) and installed for the
/// target-machine constructors, which append it after the per-target
/// default features.
#[cfg(feature = "llvm")]
pub(super) fn apply_target_features_override(resolved: Option<String>) {
    let Some(features) = resolved else { return };
    if features == "help" {
        crate::backend::driver::print_target_cpu_help();
        process::exit(0);
    }
    if let Err(msg) = crate::backend::driver::validate_target_features(&features) {
        eprintln!("{msg}");
        process::exit(1);
    }
    crate::target::set_target_features_override(&features);
}

/// Normalize a rendered `DiagnosticJson` entry for cross-target
/// comparison by dropping its run-local `"id":"dN"` field (always the
/// first field — see `DiagnosticJson::add`). An entry unique to one
/// target shifts every subsequent id in that run, so raw string
/// equality would misclassify otherwise-identical diagnostics.
/// The JSON halves of a multi-target check (design.md § Checking several
/// targets): one `{"target",…,"diagnostics"}` object per target, holding the
/// entries that are not on every target, and the `shared_diagnostics`
/// entries that are, reported once (drawn from the first target's run, ids
/// included). The dedup key is the entry minus its run-local id. With one
/// target there is nothing to compare against, so everything stays tagged.
/// Returns the comma-joined target objects and shared entries.
pub(super) fn split_target_json(runs: &[(&str, usize, &[String])]) -> (String, String) {
    let shared_keys: std::collections::HashSet<String> = if runs.len() > 1 {
        runs[0]
            .2
            .iter()
            .map(|e| strip_diag_id(e))
            .filter(|key| {
                runs[1..]
                    .iter()
                    .all(|r| r.2.iter().any(|e| strip_diag_id(e) == *key))
            })
            .collect()
    } else {
        std::collections::HashSet::new()
    };
    let shared: Vec<&str> = runs
        .first()
        .map(|r| {
            r.2.iter()
                .filter(|e| shared_keys.contains(&strip_diag_id(e)))
                .map(String::as_str)
                .collect()
        })
        .unwrap_or_default();
    let blocks: Vec<String> = runs
        .iter()
        .map(|(target, total_errors, entries)| {
            let own: Vec<&str> = entries
                .iter()
                .filter(|e| !shared_keys.contains(&strip_diag_id(e)))
                .map(String::as_str)
                .collect();
            format!(
                "{{\"target\":{},\"success\":{},\"total_errors\":{},\"diagnostics\":[{}]}}",
                json_string(target),
                *total_errors == 0,
                total_errors,
                own.join(","),
            )
        })
        .collect();
    (blocks.join(","), shared.join(","))
}

pub(super) fn strip_diag_id(entry: &str) -> String {
    let Some(rest) = entry.strip_prefix("{\"id\":\"") else {
        return entry.to_string();
    };
    match rest.find("\",") {
        Some(idx) => format!("{{{}", &rest[idx + 2..]),
        None => entry.to_string(),
    }
}

/// Multi-target check driver (phase-10, design.md § Cross-target
/// Compilation > `karac check` Under Multiple Targets). Runs the full
/// type-check + effect-check pipeline once per target, parameterizing
/// the target-provided resource set each time via
/// `target::set_active_target` — which also re-parameterizes
/// `#[target(...)]` absence filtering and tombstone diagnostics, so
/// each pass sees exactly the item set and gate that target's build
/// would see. Diagnostics are tagged with the producing target;
/// diagnostics identical on EVERY target are deduplicated into a
/// shared "all targets" group (they're target-agnostic bugs, not
/// target-specific) — text and JSON modes only; JSONL streams
/// per-target between `target_start`/`target_complete` markers and
/// leaves dedup to the consumer, mirroring the profiles driver.
/// Bounded by construction: the target set is closed at four.
pub(super) fn cmd_check_targets(
    filename: &str,
    source: &str,
    output: OutputMode,
    targets: &[String],
    lint_overrides: crate::lints::CliLintOverrides,
) {
    let mut any_failed = false;

    if let OutputMode::Jsonl = output {
        for target in targets {
            crate::target::set_active_target(target)
                .expect("target names validated at parse/manifest load");
            emit_jsonl_event(
                "target_start",
                &format!("\"target\":{}", json_string(target)),
            );
            let mut pipeline = Pipeline::new(filename, source)
                .with_lint_overrides(lint_overrides.clone())
                .strict_core()
                .with_library();
            run_pipeline_jsonl(&mut pipeline);
            let total = pipeline.total_errors();
            if total > 0 {
                any_failed = true;
            }
            emit_jsonl_event(
                "target_complete",
                &format!(
                    "\"target\":{},\"success\":{},\"total_errors\":{}",
                    json_string(target),
                    total == 0,
                    total,
                ),
            );
        }
        if any_failed {
            process::exit(1);
        }
        return;
    }

    // Text + JSON: run every target first, collecting both rendered
    // text blocks and JSON entries per target, then split shared vs
    // target-specific. Each mode dedups over its own rendering — text
    // over the rendered block (phase + span + message), JSON over the
    // entry normalized for its run-local `"id"` counter (an entry
    // unique to one target shifts every later id, so raw string
    // equality would under-dedup). The splits can differ at the
    // margin (JSON carries typecheck warnings text mode doesn't);
    // each is consistent within its own output.
    struct TargetRun {
        target: String,
        total_errors: usize,
        text_blocks: Vec<String>,
        json_entries: Vec<String>,
        skipped: std::collections::HashMap<String, String>,
    }
    let mut runs: Vec<TargetRun> = Vec::new();
    for target in targets {
        crate::target::set_active_target(target)
            .expect("target names validated at parse/manifest load");
        let mut pipeline = Pipeline::new(filename, source)
            .with_lint_overrides(lint_overrides.clone())
            .strict_core()
            .with_library();
        pipeline.run_all_checks();
        let total = pipeline.total_errors();
        if total > 0 {
            any_failed = true;
        }
        runs.push(TargetRun {
            target: target.clone(),
            total_errors: total,
            text_blocks: render_text_diagnostics(&pipeline),
            json_entries: collect_diagnostics(&pipeline).entries,
            skipped: pipeline.target_skipped.clone(),
        });
    }

    // Items NO requested target checked (B-2026-08-05-29). Reported once
    // for the whole run, not per pass — see `unchecked_across_targets`.
    let per_target: Vec<_> = runs.iter().map(|r| r.skipped.clone()).collect();
    let unchecked = unchecked_across_targets(&per_target);
    let scope = if targets.len() == 1 {
        format!("target '{}'", targets[0])
    } else {
        format!("every checked target ({})", targets.join(", "))
    };

    // A diagnostic is target-agnostic when its rendered block appears
    // on every target. Set semantics — exact duplicate blocks within
    // one target collapse, which is already redundant output. With a
    // single requested target there is nothing to compare against, so
    // everything stays target-tagged.
    let shared: Vec<String> = if runs.len() > 1 {
        runs[0]
            .text_blocks
            .iter()
            .filter(|block| runs[1..].iter().all(|r| r.text_blocks.contains(block)))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    let shared_set: std::collections::HashSet<&str> = shared.iter().map(|s| s.as_str()).collect();

    match output {
        OutputMode::Text => {
            if !shared.is_empty() {
                eprintln!("── all targets ──");
                for block in &shared {
                    eprintln!("{block}");
                }
            }
            for (idx, run) in runs.iter().enumerate() {
                if idx > 0 || !shared.is_empty() {
                    eprintln!();
                }
                eprintln!("── target: {} ──", run.target);
                for block in &run.text_blocks {
                    if shared_set.contains(block.as_str()) {
                        continue;
                    }
                    eprintln!("{block}");
                }
                if run.total_errors > 0 {
                    eprintln!(
                        "{} error(s) under target '{}'.",
                        run.total_errors, run.target
                    );
                } else {
                    eprintln!("All checks passed under target '{}'.", run.target);
                }
            }
            if let Some(note) = render_target_skip_note(&unchecked, &scope) {
                eprintln!();
                eprintln!("{note}");
            }
        }
        OutputMode::Json => {
            let per_target: Vec<(&str, usize, &[String])> = runs
                .iter()
                .map(|r| (r.target.as_str(), r.total_errors, &r.json_entries[..]))
                .collect();
            let (blocks, shared_json) = split_target_json(&per_target);
            println!(
                "{{\"targets\":[{}],\"shared_diagnostics\":[{}],\"target_skipped\":{},\"success\":{}}}",
                blocks,
                shared_json,
                // Peer to the single-pass field, same shape: items NO
                // requested target checked. An EMPTY array is meaningful —
                // it says the matrix covered everything.
                render_target_skipped_json(&unchecked),
                !any_failed,
            );
        }
        OutputMode::Jsonl => unreachable!("handled above"),
    }

    if any_failed {
        process::exit(1);
    }
}

/// Classify a `--target` value and activate it when it names a v1
/// compilation target (phase-10 WASM build path).
///
/// - v1 names: `native`, `wasm_wasi`, and `wasm_browser` are buildable —
///   the name is installed as the process-wide active target (see
///   `target::set_active_target`) so `#[target(...)]` absence semantics,
///   tombstone diagnostics, and the E0411 target gate all key on it.
///   `wasm_browser` additionally emits the `<stem>.js` ES-module glue
///   next to the `.wasm` (see `wasm_glue`). `gpu` (kernels are
///   dispatched from a host program, not standalone built) is rejected
///   loudly rather than silently producing a native binary.
/// - Anything else is a rustc-style triple — project mode's manifest
///   `[target.<triple>.*]` overlay selector — and leaves the active
///   target at `native`.
///
/// Returns the active v1 target name for the build.
pub(super) fn resolve_build_target(target: Option<&str>) -> &'static str {
    match target {
        Some(name) if crate::target::is_v1_target_name(name) => match name {
            "gpu" => {
                eprintln!(
                    "error: `--target=gpu` is not a standalone build target — GPU kernels \
                     are consumed by a host program via gpu.dispatch (design.md § Target \
                     Build Artifacts)."
                );
                process::exit(1);
            }
            buildable => {
                crate::target::set_active_target(buildable)
                    .expect("v1 target name membership checked above");
                crate::target::active_target()
            }
        },
        _ => crate::target::active_target(),
    }
}

// CLI dispatch helpers naturally land more flag-shaped arguments
// than the clippy default; factoring them into a struct here would
// just move the flag list rather than tighten it.
/// Emit a single-file-build error respecting the output mode (text to stderr,
/// a minimal one-line JSON diagnostic under `--output=json`).
pub(super) fn emit_build_error(msg: &str, output: OutputMode) {
    match output {
        OutputMode::Json | OutputMode::Jsonl => {
            println!(
                "{{\"severity\":\"error\",\"phase\":\"build\",\"concept\":\"compilation/pipeline\",\"message\":{}}}",
                json_string(msg)
            );
        }
        OutputMode::Text => eprintln!("error: {msg}"),
    }
}

/// The root of the `kara.toml` package `filename` belongs to, or `None` when
/// it is a standalone file (no package root above it, or it sits outside the
/// package's `src/`). Extracted so the two callers that care whether a file
/// has unseen sibling modules — the single-file build refusal below and the
/// resolver's `pub`-rename guard (B-2026-07-31-33) — agree on the answer.
pub(super) fn package_root_of_member(filename: &str) -> Option<PathBuf> {
    let path = std::path::Path::new(filename);
    let parent = path.parent()?;
    let file_dir = if parent.as_os_str().is_empty() {
        std::path::Path::new(".")
    } else {
        parent
    };
    let root = manifest::discover_project_root(file_dir)?;
    let abs_file = std::fs::canonicalize(path).ok()?;
    let abs_src = std::fs::canonicalize(root.join("src")).ok()?;
    abs_file.starts_with(&abs_src).then_some(root)
}

/// Whether `filename` is a module of a multi-file package, and so may have
/// readers of its `pub` items that a single-file check never sees.
pub(super) fn is_package_member(filename: &str) -> bool {
    package_root_of_member(filename).is_some()
}
