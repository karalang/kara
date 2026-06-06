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
    let handle = std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let args: Vec<String> = std::env::args().collect();
            let cmd = karac::cli::parse_args(&args);
            karac::cli::execute(cmd);
        })
        .expect("failed to spawn CLI thread");
    if let Err(payload) = handle.join() {
        std::panic::resume_unwind(payload);
    }
}

// `karac` the binary is native-only — the wasm32 build is for the
// browser playground (tracker line 703), which consumes `karac` as a
// library through the `playground/` workspace member, not as a CLI.
// The empty `main` here satisfies `[[bin]]` for wasm32 without pulling
// in the CLI surface (`std::env::args` is unavailable, and `cli` is
// cfg-gated off at lib.rs).
#[cfg(target_arch = "wasm32")]
fn main() {}
