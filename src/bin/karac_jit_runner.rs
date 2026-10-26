//! Phase-7 L560 W3.4 — subprocess helper that runs a karac-emitted
//! LLVM IR module through `LLJITEngine` and exits with the JIT'd
//! `main`'s return code.
//!
//! `karac_jit_runner <ir-path>` runs one IR module and exits with its
//! `main`'s return code, for `karac run`
//! (`run_check_cmds::run_ir_via_jit_subprocess_with`) and the corpus
//! runner's `__mir-llvm-run`. Process termination per run is the
//! panic-isolation story. The pre-M3 `--repl-mode` and `--test-batch`
//! modes, persistent engines for the legacy REPL and `karac test`, went
//! with the legacy pipeline: v2's REPL and test runner run on MIR
//! (B-2026-10-23-21).
//!
//! Exit codes:
//!   - `0..=N` — whatever the JIT'd `main` returned (0 = success,
//!     1 = `emit_panic`'s `exit(1)`, other = explicit user return).
//!   - `2` — helper setup failure (could not read IR, JIT init or
//!     `main` lookup failed). Diagnostic to stderr.

use std::process::ExitCode;

use karac::backend::lljit::LLJITEngine;

// ── KARAC_SPAWN_SITES stand-ins ──────────────────────────────────────
// Mirror of the test-binary stand-ins in `tests/codegen.rs` and
// `tests/lljit_e2e.rs`: the runtime crate declares these as `extern`
// under `#[cfg(not(test))]`, so the AOT user-binary path resolves
// them against codegen-emitted globals. JIT'd code emits its own
// per-module copies inside its JITDylib — the helper binary still
// needs satisfiers for the static rlib link of `karac-runtime`.
// `_ENABLED = 0` keeps `karac_runtime_has_debug_metadata` short-
// circuiting; `_LEN = 0` keeps the (unused) iteration paths no-op.
#[no_mangle]
#[allow(non_upper_case_globals)]
pub static KARAC_SPAWN_SITES_ENABLED: u8 = 0;
#[no_mangle]
#[allow(non_upper_case_globals)]
pub static KARAC_SPAWN_SITES_LEN: u32 = 0;
#[no_mangle]
#[allow(non_upper_case_globals)]
pub static KARAC_SPAWN_SITES: KaracSpawnSitesPad = KaracSpawnSitesPad([0; 4]);

#[repr(C, align(8))]
pub struct KaracSpawnSitesPad([u64; 4]);
unsafe impl Sync for KaracSpawnSitesPad {}

#[used]
static _FORCE_LINK_CALL_SITE: fn() -> usize = force_link_karac_runtime;

fn force_link_karac_runtime() -> usize {
    karac_runtime::__preserve_no_mangle_symbols()
}

/// Restore the SIGPIPE disposition a karac-compiled binary actually starts
/// with — B-2026-08-19-7.
///
/// THE RUNNER'S JOB IS TO BE INDISTINGUISHABLE FROM AN AOT BINARY, and this is
/// the one place where being a Rust program leaks through. A karac AOT binary's
/// `main` bypasses Rust std's runtime init, so it inherits the DEFAULT SIGPIPE
/// disposition: the kernel kills it at the first write to a closed pipe, which
/// is why `./prog | head -2` ends in 4 ms with status 141. `karac_jit_runner`
/// IS a Rust program, so `lang_start` installs `SIG_IGN` before any user code
/// runs — and then the JIT'd program's `fwrite` gets `EPIPE`, which codegen's
/// console wrapper discards, so the program runs to completion writing into a
/// pipe nobody is reading. Measured on a 2,000,000-line program: 272 ms and
/// exit 0 under the JIT against 4 ms and 141 for the AOT binary, and for a
/// program that streams without end the JIT never stops at all.
///
/// WHY THIS IS SAFE FOR SERVERS, which is the obvious objection: the runtime
/// already masks SIGPIPE for exactly the programs that need it, and does so
/// from the network reactor's one-time init (`event_loop.rs`, `event_loops()`),
/// not from process startup. So a socket-writing program re-installs `SIG_IGN`
/// itself the moment it registers its first fd — under the JIT exactly as under
/// AOT, through the same code path. This restores the AOT starting state and
/// leaves that carve-out untouched; it does not introduce a disposition the
/// AOT surface does not already have.
#[cfg(unix)]
fn restore_default_sigpipe() {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

#[cfg(not(unix))]
fn restore_default_sigpipe() {}

/// Remove the handoff IR file the spawner left for us, when the spawner asked
/// us to own it (`KARAC_JIT_IR_UNLINK`, set only by
/// `run_check_cmds::run_ir_via_jit_subprocess_with`).
///
/// B-2026-09-10-10: `oneshot_main` already unlinks, immediately after
/// `read_to_string`, and that covers every path where this process gets as far
/// as opening the file. The two `libc::_exit(129)` orphan exits below do NOT:
/// `main` calls `watch_for_parent_death()` BEFORE `oneshot_main`, so a runner
/// whose spawner is SIGKILLed while it is still doing dynamic-load /
/// arg-parsing work leaves via the startup check without ever reading the
/// file — and the spawner, being dead, cannot run its own `remove_file`
/// either. Nobody unlinks, and the ~53 KB file stays on disk indefinitely
/// (measured present 30 minutes later). Deterministically: the runner started
/// with a `SPAWNER_PID_ENV` naming a process that is not our parent exits 129
/// with the file still there.
///
/// argv[1] is the path, which is also why this declines a `--`-prefixed first
/// argument: that is a flag, not a file.
#[cfg(unix)]
fn unlink_handoff_ir_if_ours() {
    if std::env::var_os("KARAC_JIT_IR_UNLINK").is_none() {
        return;
    }
    if let Some(path) = std::env::args().nth(1) {
        if !path.starts_with("--") {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Exit when the process that spawned this runner goes away — B-2026-09-05-24.
///
/// `karac_jit_runner` is never a program in its own right: it exists only to
/// execute one IR module on behalf of a parent `karac` that is waiting on it.
/// Nothing in that arrangement made the child NOTICE the parent's death, and
/// `std::process::Child` does not kill on drop, so killing the parent left the
/// runner executing the JIT'd program forever. For a program that loops that
/// means a core pinned at 100% with no obvious handle on it — the surviving
/// process is named `karac_jit_runner`, not the program the user ran. Any
/// harness that TIMES OUT a `karac run` (mutation testing, CI, the Mend loop)
/// hits this on every timeout and silently accumulates orphans that contaminate
/// every later timing measurement on the host. That is how it was found.
///
/// The parent forwards SIGINT/SIGTERM/SIGHUP to us (see
/// `run_check_cmds::run_ir_via_jit_subprocess_with`), which covers the signals a
/// parent can handle. SIGKILL it cannot, so the child has to notice on its own
/// — and this watchdog is also the backstop for the parent dying of anything
/// else at all (its own panic, an OOM kill, the terminal going away), including
/// the few-instruction window between `spawn()` returning and the parent
/// recording our pid.
///
/// POLLING `getppid()` rather than `prctl(PR_SET_PDEATHSIG)`: pdeathsig is
/// Linux-only (macOS needs a whole kqueue `EVFILT_PROC`/`NOTE_EXIT` watch for
/// the same effect) and it must be armed by the parent between fork and exec,
/// so it would protect only the call sites that opt in. Reparenting is visible
/// identically on both platforms with no parent-side cooperation at all, so one
/// mechanism here covers every current and future spawn site. The cost is a sleeping thread and
/// up to one poll interval of latency, which is nothing against a run that was
/// going to spin forever.
///
/// `KARAC_JIT_RUNNER_NO_PARENT_WATCH=1` disables it, for the deliberate case of
/// running the runner detached from its spawner.
#[cfg(unix)]
fn watch_for_parent_death() {
    if std::env::var_os("KARAC_JIT_RUNNER_NO_PARENT_WATCH").is_some() {
        return;
    }
    // WHICH pid to watch. `getppid()` alone is a RACE, and B-2026-09-09-1 is
    // what it looks like when it loses: between the spawner's `spawn()`
    // returning and this line running, the spawner can already have died and
    // left us reparented, so `getppid()` reports the REAPER instead of the
    // process we were meant to watch. The old code then hit its `spawner <= 1`
    // guard and returned WITHOUT ARMING, on the reasoning that there was
    // "nothing left to watch" — exactly backwards, because being orphaned
    // before you look means the parent is already gone and this process should
    // be leaving. Measured under CPU load, 2 runs in 6 were orphaned
    // PERMANENTLY (alive at 120 s against the test's 15 s window), each with a
    // single thread: the watchdog had never been spawned.
    //
    // So prefer the pid the spawner DECLARED for itself. If we are no longer
    // its child by the time we look, it is gone and we exit now rather than
    // arming a watch that would never fire.
    let declared = std::env::var(karac::backend::runner::SPAWNER_PID_ENV)
        .ok()
        .and_then(|s| s.trim().parse::<libc::pid_t>().ok())
        .filter(|p| *p > 1);
    // Remove it before the JIT'd program runs: anything IT spawns would
    // otherwise inherit a pid that is not its parent's and exit immediately.
    std::env::remove_var(karac::backend::runner::SPAWNER_PID_ENV);

    // SAFETY: `getppid` is a pure read of process state and cannot fail.
    let spawner = match declared {
        Some(declared) => {
            if unsafe { libc::getppid() } != declared {
                // Orphaned before we could arm. Same exit code and same
                // reasoning as the watchdog below.
                //
                // This exit is ahead of `oneshot_main`'s unlink, and the
                // spawner is by definition gone, so nothing else will remove
                // the handoff file (B-2026-09-10-10).
                unlink_handoff_ir_if_ours();
                unsafe { libc::_exit(129) };
            }
            declared
        }
        None => {
            // Hand-launched, or spawned by a site that predates the variable:
            // fall back to the observed parent. `<= 1` stays a plain return
            // here rather than an exit, because without a declared pid there is
            // no way to tell "orphaned already" from "init really is my parent",
            // and exiting on the latter would be a regression.
            let observed = unsafe { libc::getppid() };
            if observed <= 1 {
                return;
            }
            observed
        }
    };
    let _ = std::thread::Builder::new()
        .name("karac-parent-watch".to_string())
        .stack_size(64 * 1024)
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(200));
            // SAFETY: as above. A changed parent pid means the original
            // spawner was reaped — pid reuse cannot fake this, because the
            // process we get reparented to already existed under its own pid.
            if unsafe { libc::getppid() } != spawner {
                // `_exit`, not `exit`: skip atexit handlers and every
                // destructor in a process whose JIT'd program is mid-flight
                // on another thread.
                //
                // 129 is the shell's `128 + SIGHUP` — "the thing this process
                // was attached to went away", which is exactly what happened.
                //
                // Unlink first, for the same reason as the startup check
                // above: on the paths this thread fires before the one-shot
                // read (a spawner killed during our dynamic load), the file
                // has no other owner left alive (B-2026-09-10-10).
                unlink_handoff_ir_if_ours();
                unsafe { libc::_exit(129) };
            }
        });
}

#[cfg(not(unix))]
fn watch_for_parent_death() {}

fn main() -> ExitCode {
    // Before anything else, and before the JIT'd program runs: it needs the
    // AOT signal disposition.
    restore_default_sigpipe();

    // A runner whose spawner has died has nothing left to hand its result
    // to, so it should not outlive it.
    watch_for_parent_death();

    // Belt-and-suspenders: ensure the runtime's `#[no_mangle]` symbol
    // graph is materialized in the process symbol table before the
    // JIT's process-symbol-search generator runs `dlsym`.
    let _ = force_link_karac_runtime();

    let Some(first) = std::env::args().nth(1).filter(|a| !a.starts_with("--")) else {
        eprintln!("karac_jit_runner: usage: karac_jit_runner <ir-path>");
        return ExitCode::from(2);
    };

    oneshot_main(&first)
}

/// One-shot mode (slice c.3 + W3.4). Loads `ir_path`, runs it, exits.
fn oneshot_main(ir_path: &str) -> ExitCode {
    let read = std::fs::read_to_string(ir_path);

    // B-2026-09-05-24: `karac run` hands the IR over through a temp file that
    // IT removes once we exit — which never happens when it is killed before
    // we do, so every interrupted run leaked a `/tmp/karac_run_<pid>_jit.ll`.
    // The bytes are ours the moment the read returns, so unlink here instead
    // and let the parent's own `remove_file` be the no-op it now is. Opt-in via
    // the env var rather than unconditional: the other caller
    // (`__mir-llvm-run`) passes a path it manages itself, and a runner that deletes its caller's files would be
    // a surprise in the other direction.
    if std::env::var_os("KARAC_JIT_IR_UNLINK").is_some() {
        let _ = std::fs::remove_file(ir_path);
    }

    let ir = match read {
        Ok(s) => s,
        Err(e) => {
            eprintln!("karac_jit_runner: read IR {ir_path}: {e}");
            return ExitCode::from(2);
        }
    };

    if let Err(e) = load_link_libraries() {
        eprintln!("karac_jit_runner: {e}");
        return ExitCode::from(2);
    }

    let engine = match LLJITEngine::new() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("karac_jit_runner: LLJITEngine::new: {e}");
            return ExitCode::from(2);
        }
    };

    if let Err(e) = engine.add_ir_module(&ir) {
        eprintln!("karac_jit_runner: add_ir_module: {e}");
        return ExitCode::from(2);
    }

    publish_spawn_sites(&engine);

    let addr = match engine.lookup_address("main") {
        Ok(a) => a,
        Err(e) => {
            eprintln!("karac_jit_runner: lookup_address(\"main\"): {e}");
            return ExitCode::from(2);
        }
    };

    let rc: i32 = call_main(addr);

    // Flush the JIT'd program's stdio before the process exits. The
    // program's `println` lowers to libc `fwrite`/`printf` against the
    // process-resolved `stdout` FILE*, which is fully buffered when
    // stdout is a pipe (the norm under `Command::output` capture and
    // `karac run … | …`). It had been relying on libc's implicit at-exit
    // flush. That implicit flush does
    // NOT reliably run here on Linux/glibc — returning `ExitCode` from
    // `main` unwinds through Rust's lang-start rather than a plain libc
    // `exit()`, so the buffered bytes are dropped and every executed
    // program appears to print nothing (empty stdout, correct exit code).
    // macOS masked this. Flush libc's buffers (and Rust's, for symmetry)
    // explicitly so `karac run` and the codegen/lljit E2E suites capture
    // output on every platform.
    unsafe {
        libc::fflush(std::ptr::null_mut());
    }
    use std::io::Write;
    let _ = std::io::stdout().lock().flush();
    let _ = std::io::stderr().lock().flush();

    let code: u8 = if (0..=255).contains(&rc) {
        rc as u8
    } else {
        255
    };
    ExitCode::from(code)
}

fn publish_spawn_sites(engine: &LLJITEngine) {
    // W3.5: publish the JIT module's `KARAC_SPAWN_SITES*` addresses
    // into the runtime so introspection reads see the JIT module's
    // values instead of the helper bin's stand-in zeros. Best-effort:
    // each lookup falls back to null if the symbol isn't found.
    let enabled_p = engine
        .lookup_address("KARAC_SPAWN_SITES_ENABLED")
        .ok()
        .map(|a| a as *const u8)
        .unwrap_or(std::ptr::null());
    let len_p = engine
        .lookup_address("KARAC_SPAWN_SITES_LEN")
        .ok()
        .map(|a| a as *const u32)
        .unwrap_or(std::ptr::null());
    let base_p = engine
        .lookup_address("KARAC_SPAWN_SITES")
        .ok()
        .map(|a| a as *const u8)
        .unwrap_or(std::ptr::null());
    // SAFETY: addresses come from `LLVMOrcLLJITLookup` and reference
    // symbols inside the live JITDylib; the engine outlives this call.
    unsafe {
        karac_runtime::karac_runtime_init_jit_spawn_sites(enabled_p, len_p, base_p);
    }
}

fn call_main(addr: u64) -> i32 {
    // SAFETY: `addr` is the JIT-resolved address of an LLVM-emitted
    // function with C ABI signature `fn() -> i32` (the Kāra entry
    // shape per `functions.rs`). The engine outlives this call.
    unsafe {
        type MainFn = unsafe extern "C" fn() -> i32;
        let main_fn: MainFn = std::mem::transmute(addr as usize);
        main_fn()
    }
}

/// Opens the libraries of the program's `[link]` table, which `karac` hands
/// over in `KARAC_JIT_LINK_LIBS` / `KARAC_JIT_LINK_PATHS`, into this
/// process's symbols, where the JIT looks for an `extern "C"` function the
/// program calls (B-2026-10-22-10).
fn load_link_libraries() -> Result<(), String> {
    let Ok(libs) = std::env::var("KARAC_JIT_LINK_LIBS") else {
        return Ok(());
    };
    let split = |s: String| -> Vec<String> {
        s.split('\u{1F}')
            .filter(|x| !x.is_empty())
            .map(str::to_string)
            .collect()
    };
    let paths = std::env::var("KARAC_JIT_LINK_PATHS").unwrap_or_default();
    karac::target::set_native_link_config(split(libs), split(paths));
    karac::mir::foreign::load_link_libraries_globally()
}
