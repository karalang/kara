//! Slice c-repl.B.B — integration tests for the REPL JIT dispatch.
//!
//! Drives `Session::evaluate_cell_captured` with JIT mode enabled.
//! The runner subprocess is located via `KARAC_JIT_RUNNER`, which we
//! point at `env!("CARGO_BIN_EXE_karac_jit_runner")` so cargo's
//! per-test build of the runner is what we exercise. Each test sets
//! its own session (no parallel-test contention on the runner).
//!
//! What these tests pin:
//! - JIT mode flips the cell path: stdout matches the interpreter's
//!   for trivial cells, with the captured-output framing intact.
//! - Item definitions span cells via source replay (the existing
//!   non-JIT path's accumulation works under JIT too).
//! - A panicking cell trips the runner-died re-spawn flow; the next
//!   cell sees a fresh runner.

#![cfg(feature = "lljit_prototype")]

use karac::repl::Session;

/// Tell the JIT client where to find the runner binary cargo just
/// built. `current_exe().parent()` from inside the test binary points
/// at `target/<profile>/deps/`, but `karac_jit_runner` lives at
/// `target/<profile>/karac_jit_runner` — one level up. The env var
/// short-circuits `locate_runner_binary`'s search.
///
/// SAFETY: Rust 2024 made `set_var` `unsafe` because it can race
/// with other threads reading env. Tests in this file are
/// single-threaded with respect to KARAC_JIT_RUNNER — each sets the
/// same value, no read-then-write hazards.
fn enable_jit(session: &mut Session) {
    let path = env!("CARGO_BIN_EXE_karac_jit_runner");
    // Safe because: same value every test, set before any spawn.
    unsafe { std::env::set_var("KARAC_JIT_RUNNER", path) };
    session.set_jit_enabled_for_tests(true);
    assert!(
        session.jit_enabled(),
        "set_jit_enabled_for_tests didn't stick"
    );
}

#[test]
fn repl_jit_prints_a_single_cell() {
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured("println(42);");
    assert!(
        r.errors.is_empty(),
        "expected clean run; got errors: {:?}",
        r.errors
    );
    assert_eq!(
        r.stdout.trim(),
        "42",
        "expected captured '42' on stdout; full stdout: {:?}",
        r.stdout
    );
}

#[test]
fn repl_jit_persists_items_across_cells() {
    // Items accumulate via source replay (the existing non-JIT
    // mechanism). Each cell's synthetic source contains every prior
    // fn/struct definition, so cell 2's call to `double` resolves
    // against cell 1's `fn double` re-emitted into cell 2's program.
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured("fn double(n: i64) -> i64 { n * 2 }");
    assert!(r.errors.is_empty(), "fn def: {:?}", r.errors);
    let r = s.evaluate_cell_captured("println(double(7));");
    assert!(r.errors.is_empty(), "call: {:?}", r.errors);
    assert_eq!(r.stdout.trim(), "14");
}

#[test]
fn repl_jit_panic_kills_runner_and_next_cell_respawns() {
    // assert_eq mismatch trips emit_panic → exit(1). The runner dies
    // mid-cell; the client returns RunnerDied; the Session drops the
    // client. Next cell spawns a fresh runner — the user's `println`
    // in cell 3 still prints, against a clean engine.
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured("assert_eq(1, 2);");
    // Cell 1 fails — should NOT be error-free.
    assert!(
        !r.errors.is_empty(),
        "expected errors from panicking cell; stdout={:?}",
        r.stdout
    );
    let joined = r.errors.join(" ");
    assert!(
        joined.contains("died mid-cell") || joined.contains("subprocess died"),
        "expected runner-died diagnostic; got errors: {:?}",
        r.errors
    );
    // Cell 2: clean run, fresh runner.
    let r = s.evaluate_cell_captured("println(99);");
    assert!(
        r.errors.is_empty(),
        "cell after panic should run cleanly; got errors: {:?}",
        r.errors
    );
    assert_eq!(r.stdout.trim(), "99");
}

#[test]
fn repl_jit_runs_let_bindings() {
    // Persistent-let replay: cell 1 introduces `let x = 7;`, cell 2
    // references `x`. The Session's source-replay machinery re-emits
    // `let x = 7;` into cell 2's synthetic main. JIT path runs the
    // replayed source unchanged (no value-snapshot semantics yet —
    // RHS re-runs each cell, but for a literal that's invisible).
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured("let x = 7;");
    assert!(r.errors.is_empty(), "let: {:?}", r.errors);
    let r = s.evaluate_cell_captured("println(x + 1);");
    assert!(r.errors.is_empty(), "use: {:?}", r.errors);
    assert_eq!(r.stdout.trim(), "8");
}
