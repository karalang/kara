//! Finding the `karac_jit_runner` beside `karac`, and the variable that
//! tells it who spawned it. The MIR backend's `__mir-llvm-run` uses both, as
//! do `karac test`'s JIT dispatch and the REPL, which re-export them from
//! here (moved out of `test_jit_dispatch.rs` for B-2026-10-22-24).

use std::path::PathBuf;

/// Environment variable carrying the SPAWNER's own pid to `karac_jit_runner`,
/// set by every site that spawns it.
///
/// The runner's parent-death watchdog cannot learn this from `getppid()`
/// alone, because that is a RACE (B-2026-09-09-1): between `spawn()` returning
/// and the runner reaching its watchdog setup, the spawner can already have
/// died and left the runner reparented — and `getppid()` then reports the
/// REAPER rather than the process the runner was meant to watch. The watchdog
/// read that as "no meaningful parent, nothing to watch" and returned without
/// arming, which is exactly backwards: being orphaned before you look means the
/// parent is already gone. Measured under load, 2 runs in 6 were orphaned
/// PERMANENTLY (still alive at 120 s, against a 15 s test window), each with
/// only one thread — the watchdog had never been spawned.
///
/// The runner REMOVES this from its own environment once read, so a process the
/// JIT'd program itself spawns cannot inherit a stale pid and exit on it.
pub const SPAWNER_PID_ENV: &str = "KARAC_JIT_RUNNER_SPAWNER_PID";

/// Look for `karac_jit_runner`: the `KARAC_JIT_RUNNER` env override wins
/// (used by tests that build the runner at a known path and by advanced
/// installs), else the sibling of the current `karac` executable. Cargo
/// writes both binaries next to each other (target/release/karac,
/// target/release/karac_jit_runner); installed `karac` users get them
/// paired through the same install step (the `reference_karac_install_path`
/// memory pins how this is done). `pub(crate)` so `cmd_run`'s Slice-6b
/// JIT-run path shares one locator with the test batch runner.
pub fn locate_karac_jit_runner() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("KARAC_JIT_RUNNER") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    let karac_exe = std::env::current_exe().ok()?;
    let dir = karac_exe.parent()?;
    let candidate = dir.join("karac_jit_runner");
    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}
