#[cfg(not(target_arch = "wasm32"))]
fn main() {
    // Run the entire CLI on a fat-stack thread. The compiler phases are
    // deeply recursive (recursive-descent parser; tree-walk interpreter
    // at ~8 Rust frames per Kāra call with large match-on-AST frames in
    // debug builds), and the OS main-thread stack is platform lottery —
    // 1 MB on Windows vs 8 MB on Linux/macOS — so `karac.exe` overflowed
    // on programs that pass everywhere else (154 windows-CI cli-test
    // failures, run 27050301501). 16 MB matches `run_on_interp_thread`
    // (lib.rs), which already applies the same fix to the library entry
    // points; this extends it to the binary itself. `process::exit`
    // calls inside `execute` are process-wide, so exit codes are
    // unaffected; a panic unwinds to `join` and is re-raised so the
    // process still dies loudly with the original payload.
    quiet_broken_pipe();
    let handle = std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            // An argument that is not UTF-8 reaches the program with each
            // invalid sequence as U+FFFD, as in a built binary, where
            // `args()` panicked (B-2026-11-17-60).
            let args: Vec<String> = std::env::args_os()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            let cmd = karac::cli::parse_args(&args);
            karac::cli::execute(cmd);
        })
        .expect("failed to spawn CLI thread");
    if let Err(payload) = handle.join() {
        std::panic::resume_unwind(payload);
    }
}

/// A closed reader on karac's own stdout (`karac check f.kara | head`) is a
/// quiet end, not a crash: `println!` panics on EPIPE, and the hook turns
/// that panic into the process ending as the reader's SIGPIPE would end it
/// (128 + 13 = 141 to the shell, so `set -o pipefail` still sees it), with
/// no message. The interpreter's `console_written` does the same for a
/// program's output. Review-thread ruling 2026-11-16 (B-2026-11-16-224).
#[cfg(not(target_arch = "wasm32"))]
fn quiet_broken_pipe() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| info.payload().downcast_ref::<&str>().copied())
            .unwrap_or("");
        if msg.contains("Broken pipe") && msg.contains("failed printing to") {
            #[cfg(unix)]
            {
                extern "C" {
                    fn signal(sig: i32, handler: usize) -> usize;
                    fn raise(sig: i32) -> i32;
                }
                const SIGPIPE: i32 = 13;
                const SIG_DFL: usize = 0;
                // SAFETY: restoring a signal's default disposition, then
                // raising the signal it ends the process on.
                unsafe {
                    signal(SIGPIPE, SIG_DFL);
                    raise(SIGPIPE);
                }
            }
            std::process::exit(141);
        }
        default(info);
    }));
}

// `karac` the binary is native-only — the wasm32 build is for the
// browser playground (tracker line 703), which consumes `karac` as a
// library through the `playground/` workspace member, not as a CLI.
// The empty `main` here satisfies `[[bin]]` for wasm32 without pulling
// in the CLI surface (`std::env::args` is unavailable, and `cli` is
// cfg-gated off at lib.rs).
#[cfg(target_arch = "wasm32")]
fn main() {}
