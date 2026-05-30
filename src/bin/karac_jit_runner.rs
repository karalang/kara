//! Phase-7 L560 W3.4 — subprocess helper that runs a karac-emitted
//! LLVM IR module through `LLJITEngine` and exits with the JIT'd
//! `main`'s return code.
//!
//! Two modes:
//!
//!   - **One-shot** (`karac_jit_runner <ir-path>`): the W3.4 form used
//!     by `tests/codegen.rs::jit_dispatch` and `cmd_test`'s JIT
//!     dispatch (slice c.3). Runs one IR module, exits with its
//!     `main`'s return code. Process termination per cell is the
//!     panic-isolation story.
//!
//!   - **REPL** (`karac_jit_runner --repl-mode`): the Option B
//!     persistent-subprocess form (slice c-repl.B.A). Holds a single
//!     `LLJITEngine` across cells; reads framed `cell <id>
//!     <ir_byte_count>\n<ir bytes>` commands from stdin, installs each
//!     under its own `ResourceTracker`, redirects stdout/stderr to
//!     per-cell tempfiles via dup2 so the captured bytes can be
//!     framed back to the parent. Outcome wire shape:
//!
//!         result <id> <exit> <stdout_byte_count> <stderr_byte_count>\n
//!         <stdout bytes><stderr bytes>
//!
//!     `quit\n` shuts down. EOF on stdin shuts down. Panics inside a
//!     cell terminate the runner (the JIT'd `emit_panic` does
//!     `printf + exit(1)`); the parent re-spawns and replays prior
//!     cells if it wants to continue.
//!
//! Exit codes (one-shot only — repl mode exits 0 on graceful shutdown):
//!   - `0..=N` — whatever the JIT'd `main` returned (0 = success,
//!     1 = `emit_panic`'s `exit(1)`, other = explicit user return).
//!   - `2` — helper setup failure (could not read IR, JIT init or
//!     `main` lookup failed). Diagnostic to stderr.

use std::io::{BufRead, Read, Write};
use std::process::ExitCode;

use karac::codegen::LLJITEngine;

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

fn main() -> ExitCode {
    // Belt-and-suspenders: ensure the runtime's `#[no_mangle]` symbol
    // graph is materialized in the process symbol table before the
    // JIT's process-symbol-search generator runs `dlsym`.
    let _ = force_link_karac_runtime();

    let mut args = std::env::args();
    let _prog = args.next();
    let first = match args.next() {
        Some(s) => s,
        None => {
            eprintln!("karac_jit_runner: missing argv[1] (either an IR path or --repl-mode)");
            return ExitCode::from(2);
        }
    };

    if first == "--repl-mode" {
        return repl_main();
    }

    oneshot_main(&first)
}

/// One-shot mode (slice c.3 + W3.4). Loads `ir_path`, runs it, exits.
fn oneshot_main(ir_path: &str) -> ExitCode {
    let ir = match std::fs::read_to_string(ir_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("karac_jit_runner: read IR {ir_path}: {e}");
            return ExitCode::from(2);
        }
    };

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

    let code: u8 = if (0..=255).contains(&rc) {
        rc as u8
    } else {
        255
    };
    ExitCode::from(code)
}

/// REPL mode (slice c-repl.B.A). Persistent engine; reads framed cell
/// commands from stdin; writes framed responses to stdout. Stdout is
/// the bidirectional response channel — anything the runner needs to
/// say *outside* the framed protocol goes to stderr.
fn repl_main() -> ExitCode {
    let engine = match LLJITEngine::new() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("karac_jit_runner: LLJITEngine::new: {e}");
            return ExitCode::from(2);
        }
    };

    // Acknowledge readiness so the parent's first send doesn't race
    // engine init. Parent reads this single line before sending the
    // first `cell` command.
    {
        let stdout = std::io::stdout();
        let mut stdout_lock = stdout.lock();
        let _ = writeln!(stdout_lock, "ready");
        let _ = stdout_lock.flush();
    }

    // Track the most recently installed cell's tracker. Per W2's
    // shadowing finding: every karac-emitted module exports the same
    // `main` symbol (plus the Debugger-Contract `KARAC_SPAWN_SITES*`
    // globals) — installing a second module trips
    // "Duplicate definition of symbol 'main'" unless the prior
    // tracker's `.remove()` is called first. The cell-shadowing
    // pattern from the W2 stress test handles this: each cell holds
    // its own tracker; the prior one is removed right before the
    // next install. Cross-cell symbol visibility for additive cells
    // (cell 2 references cell 1's `fn foo`) is a follow-on slice —
    // it requires codegen to emit prior items as `declare` rather
    // than `define`, plus a registry of which symbols are already
    // live in the JITDylib.
    let mut prior_tracker: Option<karac::codegen::ResourceTracker<'_>> = None;

    let stdin = std::io::stdin();
    let mut stdin_lock = stdin.lock();

    loop {
        let mut header = String::new();
        let n = match stdin_lock.read_line(&mut header) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("karac_jit_runner: stdin read: {e}");
                return ExitCode::from(2);
            }
        };
        if n == 0 {
            // EOF — parent closed stdin. Clean exit.
            return ExitCode::from(0);
        }
        let trimmed = header.trim_end_matches(['\r', '\n']);
        if trimmed == "quit" {
            return ExitCode::from(0);
        }

        // Parse "cell <id> <ir_byte_count>"
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() != 3 || parts[0] != "cell" {
            write_protocol_error(&format!("unrecognized command: {trimmed:?}"));
            continue;
        }
        let id: u64 = match parts[1].parse() {
            Ok(v) => v,
            Err(_) => {
                write_protocol_error(&format!("bad cell id: {:?}", parts[1]));
                continue;
            }
        };
        let ir_len: usize = match parts[2].parse() {
            Ok(v) => v,
            Err(_) => {
                write_protocol_error(&format!("bad ir byte count: {:?}", parts[2]));
                continue;
            }
        };

        let mut ir_buf = vec![0u8; ir_len];
        if let Err(e) = stdin_lock.read_exact(&mut ir_buf) {
            eprintln!("karac_jit_runner: failed to read {ir_len} IR bytes: {e}");
            return ExitCode::from(2);
        }
        let ir = match String::from_utf8(ir_buf) {
            Ok(s) => s,
            Err(_) => {
                write_cell_setup_error(id, "IR bytes were not valid UTF-8");
                continue;
            }
        };

        // Remove the prior cell's module before installing the new
        // one — see the comment at `prior_tracker`'s declaration for
        // the shadowing rationale.
        if let Some(t) = prior_tracker.take() {
            if let Err(e) = t.remove() {
                write_cell_setup_error(id, &format!("prior tracker remove: {e}"));
                continue;
            }
        }

        let (outcome, new_tracker) = run_one_cell(&engine, &ir);
        prior_tracker = new_tracker;
        write_cell_outcome(id, &outcome);
    }
}

/// Wraps the per-cell install/capture/execute cycle. `setup_err` is
/// `Some` when something went wrong before `main` could run (IR parse
/// failed, no `main` symbol, etc.) — caller surfaces it on stderr in
/// the framed response. Otherwise `exit` is the JIT'd `main`'s return
/// value.
struct CellOutcome {
    exit: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn run_one_cell<'a>(
    engine: &'a LLJITEngine,
    ir: &str,
) -> (CellOutcome, Option<karac::codegen::ResourceTracker<'a>>) {
    let tracker = match engine.add_ir_module_with_tracker(ir) {
        Ok(t) => t,
        Err(e) => {
            return (
                CellOutcome {
                    exit: 2,
                    stdout: Vec::new(),
                    stderr: format!("karac_jit_runner: add_ir_module: {e}\n").into_bytes(),
                },
                None,
            );
        }
    };
    publish_spawn_sites(engine);
    let addr = match engine.lookup_address("main") {
        Ok(a) => a,
        Err(e) => {
            return (
                CellOutcome {
                    exit: 2,
                    stdout: Vec::new(),
                    stderr: format!("karac_jit_runner: lookup main: {e}\n").into_bytes(),
                },
                Some(tracker),
            );
        }
    };

    // Redirect stdout/stderr to tempfiles so the JIT'd `printf` calls
    // land in per-cell buffers we can frame back to the parent. See
    // also `tests/lljit_e2e.rs` for the pre-W3.4 in-process capture
    // path that this mirrors.
    let captured = capture_via_redirect(|| call_main(addr));

    (
        CellOutcome {
            exit: captured.rc,
            stdout: captured.stdout,
            stderr: captured.stderr,
        },
        // Return the tracker so the caller can release it before
        // the next cell installs (shadowing the `main` symbol).
        Some(tracker),
    )
}

struct CapturedRun {
    rc: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn capture_via_redirect<F: FnOnce() -> i32>(f: F) -> CapturedRun {
    // We use dup2 to redirect fds 1 + 2 to tempfiles. The dance is
    // safe because all JIT'd printf calls go through libc, which
    // writes to fd 1 / 2 directly; Rust's `println!` uses its own
    // buffered stream, but we don't expect the JIT'd code to emit
    // via that path. Buffers are flushed at both ends of the
    // redirect.

    // SAFETY: dup/dup2/close are POSIX fd primitives; the calls
    // below preserve their preconditions (valid fd argument, the
    // saved fds are closed exactly once after restoration).
    use std::os::fd::RawFd;

    fn dup_fd(fd: RawFd) -> RawFd {
        unsafe { libc::dup(fd) }
    }
    fn dup2_fd(src: RawFd, dst: RawFd) {
        unsafe {
            libc::dup2(src, dst);
        }
    }
    fn close_fd(fd: RawFd) {
        unsafe {
            libc::close(fd);
        }
    }

    // Flush before redirect so any prior output goes to the real
    // streams, not the per-cell tempfile.
    unsafe {
        libc::fflush(std::ptr::null_mut());
    }
    let _ = std::io::stdout().lock().flush();
    let _ = std::io::stderr().lock().flush();

    let saved_stdout = dup_fd(1);
    let saved_stderr = dup_fd(2);

    let stdout_tmp = match tempfile_for_redirect("karac_jit_runner_cell_stdout") {
        Ok(f) => f,
        Err(e) => {
            return CapturedRun {
                rc: 2,
                stdout: Vec::new(),
                stderr: format!("karac_jit_runner: stdout tempfile: {e}\n").into_bytes(),
            };
        }
    };
    let stderr_tmp = match tempfile_for_redirect("karac_jit_runner_cell_stderr") {
        Ok(f) => f,
        Err(e) => {
            return CapturedRun {
                rc: 2,
                stdout: Vec::new(),
                stderr: format!("karac_jit_runner: stderr tempfile: {e}\n").into_bytes(),
            };
        }
    };
    let stdout_fd = stdout_tmp.fd;
    let stderr_fd = stderr_tmp.fd;
    let stdout_path = stdout_tmp.path.clone();
    let stderr_path = stderr_tmp.path.clone();

    dup2_fd(stdout_fd, 1);
    dup2_fd(stderr_fd, 2);
    close_fd(stdout_fd);
    close_fd(stderr_fd);

    let rc = f();

    // Flush again so JIT'd output reaches the tempfile before we
    // restore the saved fds.
    unsafe {
        libc::fflush(std::ptr::null_mut());
    }

    dup2_fd(saved_stdout, 1);
    dup2_fd(saved_stderr, 2);
    close_fd(saved_stdout);
    close_fd(saved_stderr);

    let stdout = std::fs::read(&stdout_path).unwrap_or_default();
    let stderr = std::fs::read(&stderr_path).unwrap_or_default();
    let _ = std::fs::remove_file(&stdout_path);
    let _ = std::fs::remove_file(&stderr_path);

    CapturedRun { rc, stdout, stderr }
}

struct RedirectTempfile {
    path: std::path::PathBuf,
    fd: std::os::fd::RawFd,
}

fn tempfile_for_redirect(prefix: &str) -> std::io::Result<RedirectTempfile> {
    use std::os::fd::IntoRawFd;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "{prefix}_{}_{}",
        std::process::id(),
        id
    ));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)?;
    let fd = file.into_raw_fd();
    Ok(RedirectTempfile { path, fd })
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

/// Write a framed result response on fd 1 (stdout). Buffered locally
/// then written in one shot so the parent's `read` sees the whole
/// frame.
fn write_cell_outcome(id: u64, outcome: &CellOutcome) {
    let header = format!(
        "result {} {} {} {}\n",
        id,
        outcome.exit,
        outcome.stdout.len(),
        outcome.stderr.len()
    );
    let stdout = std::io::stdout();
    let mut stdout_lock = stdout.lock();
    let _ = stdout_lock.write_all(header.as_bytes());
    let _ = stdout_lock.write_all(&outcome.stdout);
    let _ = stdout_lock.write_all(&outcome.stderr);
    let _ = stdout_lock.flush();
}

fn write_cell_setup_error(id: u64, msg: &str) {
    let bytes = msg.as_bytes();
    let header = format!("result {} 2 0 {}\n", id, bytes.len() + 1);
    let stdout = std::io::stdout();
    let mut stdout_lock = stdout.lock();
    let _ = stdout_lock.write_all(header.as_bytes());
    let _ = stdout_lock.write_all(bytes);
    let _ = stdout_lock.write_all(b"\n");
    let _ = stdout_lock.flush();
}

fn write_protocol_error(msg: &str) {
    eprintln!("karac_jit_runner: protocol error: {msg}");
}
