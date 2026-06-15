//! Shared spawn-watchdog helper for integration tests.
//!
//! Lifted from `tests/codegen.rs` (commit `62af025`) and reused across the
//! other test files that spawn user-compiled binaries: `tests/par_codegen.rs`,
//! `tests/parallax.rs`, `tests/parallax_lite.rs`, `tests/cli.rs`. The original
//! incident was a `cargo test --features llvm --test codegen` hang of 30+ min
//! traced to a concurrent `Command::output()` deadlock under `cargo test`
//! parallelism — the four files above have the same structural exposure
//! (parallel `.output()` invocations on child binaries sharing pipe fds).
//! Tracker entry that filed this mirror: `phase-7-codegen.md` § *Mirror
//! `output_with_hang_watchdog` into ...*.
//!
//! Per-file timeout calibration (the slice plan's only twist):
//! - 15 s for short-lived helpers (`codegen.rs`, `cli.rs`)
//! - 60 s for parallel-workload binaries (`par_codegen.rs`, `parallax.rs`,
//!   `parallax_lite.rs`) — they intentionally run 5-15 s of real work and
//!   need headroom above that to distinguish "slow under load" from "hung".
//!
//! Rust's integration-test layout requires this module live at
//! `tests/common/mod.rs` (not `tests/common.rs`), otherwise cargo treats it
//! as another test binary. `mod common;` from each test file picks it up.

#![allow(dead_code)]

use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Spawn `cmd`, capture stdout/stderr, and kill the child if it hasn't
/// finished within `timeout`. Returns `None` if the spawn itself failed
/// (so callers can soft-skip), panics if the child was killed for hanging
/// (so a CI run surfaces the hang clearly instead of silently passing on
/// a partial output).
///
/// Same shape as the original inline helper in `tests/codegen.rs` pre-
/// extraction: stdin redirected from /dev/null, stdout+stderr piped,
/// `kill -9 <pid>` on timeout, watchdog thread joined before return so
/// the kill is observable on stderr.
pub fn output_with_hang_watchdog(mut cmd: Command, timeout: Duration) -> Option<Output> {
    // Bound each child binary's auto-par worker pool so a suite-wide run does
    // not oversubscribe the machine. `cargo test` runs ~`num_cpus` test threads
    // in parallel, and since the 2026-06-14 auto-par ordered-output change far
    // more E2E programs now spawn the runtime's work-stealing pool (output-
    // bearing mains are no longer suppressed). Left uncapped, each child spins
    // `available_parallelism()` (~18) workers → `test_threads × 18` threads
    // thrash a `num_cpus`-core box, and child binaries miss the watchdog's
    // timeout (slow-under-load read as "hung"). Two workers still exercises the
    // real multi-branch par_run path (queue + work-helping join + ordered-
    // output capture/replay) while keeping the total thread count bounded.
    // Honors an explicit caller override (e.g. a wall-clock benchmark that
    // wants full width) — only sets the default when unset.
    let workers_key = std::ffi::OsStr::new("KARAC_PAR_WORKERS");
    if !cmd.get_envs().any(|(k, _)| k == workers_key) {
        cmd.env("KARAC_PAR_WORKERS", "2");
    }
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let pid = child.id();

    let (tx, rx) = mpsc::channel::<()>();
    let watchdog = std::thread::spawn(move || {
        if rx.recv_timeout(timeout).is_err() {
            eprintln!(
                "FATAL: test child (pid {pid}) hung for >{}s — killing. \
                 Likely a concurrent Command::output() deadlock under \
                 cargo test parallelism. Re-run with `--test-threads=1` \
                 to isolate, or run the failing test alone.",
                timeout.as_secs(),
            );
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
            true
        } else {
            false
        }
    });

    let result = child.wait_with_output().ok();
    let _ = tx.send(());
    let killed = watchdog.join().unwrap_or(false);

    if killed {
        panic!("test child binary hung — see stderr above for diagnostics");
    }
    result
}
