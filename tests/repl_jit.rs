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

#[test]
fn repl_jit_declare_only_linkage_across_three_cells() {
    // Slice c-repl.B.4 latent-bug probe: cell 1 defines a fn via the
    // pure-items path; cell 2 runs through JIT and registers the fn
    // in `jit_installed_fns` (so its body is now live in the runner's
    // JITDylib); cell 3 hits the declare-only emission path for that
    // fn. B.4's `declare_function` applies `Linkage::Internal` for
    // non-pub fns, but Internal linkage requires a body in the SAME
    // module — for declare-only it must be External. Before the fix,
    // cell 3 fails LLVM verifier with `Global is external, but doesn't
    // have external or weak linkage!`. Existing B.4 tests are 2-cell
    // so they never tripped this. Fixed in B.5.1 alongside the
    // value-snapshot port (the snapshot test depends on this path).
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured("fn note() -> i64 { 42 }");
    assert!(r.errors.is_empty(), "cell 1 (item): {:?}", r.errors);
    let r = s.evaluate_cell_captured("println(note());");
    assert!(r.errors.is_empty(), "cell 2 (use): {:?}", r.errors);
    assert_eq!(r.stdout.trim(), "42");
    let r = s.evaluate_cell_captured("println(note() + 1);");
    assert!(r.errors.is_empty(), "cell 3 (declare-only): {:?}", r.errors);
    assert_eq!(r.stdout.trim(), "43");
}

#[test]
fn repl_jit_let_rhs_is_not_re_evaluated() {
    // Slice c-repl.B.5.1 — value-snapshot port for primitive let
    // bindings. Cell 1 binds `let x = side_effecting_fn()`; cell 2
    // references `x`. The interpreter caches the bound value, so
    // cell 2 does NOT re-run `side_effecting_fn()`. Before B.5.1 the
    // JIT path re-evaluated the RHS in cell 2 (the synthetic source
    // re-emits the let into cell 2's main, and codegen lowered it
    // verbatim). B.5.1 routes primitive-typed lets through a per-
    // binding LLVM global as a cross-cell side channel: cell 1's
    // codegen emits a store to the global; cell 2's codegen replays
    // the let by loading from the same global instead of re-running
    // the original RHS. End result: `side_effecting_fn`'s `println`
    // fires exactly once, matching the interpreter path.
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured("fn note() -> i64 { println(\"called\"); 42 }");
    assert!(r.errors.is_empty(), "fn def: {:?}", r.errors);
    let r = s.evaluate_cell_captured("let x = note();");
    assert!(r.errors.is_empty(), "let cell: {:?}", r.errors);
    assert_eq!(
        r.stdout.trim(),
        "called",
        "let cell should print the side effect once",
    );
    let r = s.evaluate_cell_captured("println(x);");
    assert!(r.errors.is_empty(), "use cell: {:?}", r.errors);
    assert_eq!(
        r.stdout.trim(),
        "42",
        "use cell should print only `x`'s cached value — `note()` must NOT re-run",
    );
}

#[test]
fn repl_jit_string_let_rhs_is_not_re_evaluated() {
    // Slice c-repl.B.5.2 — extend B.5.1's value-snapshot mechanism to
    // String bindings. Cell 1 defines a side-effecting fn that
    // allocates + returns a String and binds the result via
    // `let s = note();`; cell 2 references `s`. The interpreter
    // caches the bound value, so cell 2 must NOT re-run `note()`.
    // Pre-B.5.2 the JIT path re-evaluated the RHS on the replay cell
    // (Strings hadn't been wired into the snapshot mechanism yet),
    // so "called" printed twice. B.5.2 routes String lets through a
    // per-binding LLVM global holding the (ptr, len, cap) triple
    // and suppresses the let's scope-exit cleanup so the buffer
    // survives the cell boundary.
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured(
        "fn note() -> String { \
            println(\"called\"); \
            let mut out: String = String.new(); \
            out.push_str(\"hi\"); \
            out \
         }",
    );
    assert!(r.errors.is_empty(), "fn def: {:?}", r.errors);
    let r = s.evaluate_cell_captured("let s: String = note();");
    assert!(r.errors.is_empty(), "let cell: {:?}", r.errors);
    assert_eq!(
        r.stdout.trim(),
        "called",
        "let cell should print the side effect once",
    );
    let r = s.evaluate_cell_captured("println(s);");
    assert!(r.errors.is_empty(), "use cell: {:?}", r.errors);
    assert_eq!(
        r.stdout.trim(),
        "hi",
        "use cell should print only `s`'s cached value — `note()` must NOT re-run",
    );
}

#[test]
fn repl_jit_string_cross_cell_shadow_drops_runner() {
    // Slice c-repl.B.5.2 — cross-cell String shadow must reach the
    // same runner-drop cleanup path the primitive case uses. The
    // B.5.1 follow-up extended `prune_shadowed_lets` to drop the
    // runner whenever a new cell rebinds a name that's in
    // `jit_snapshotted_lets`; String entries land in that same map
    // so the existing shadow detection picks them up uniformly.
    // Without the drop, cell 2's snapshot global would still hold
    // cell 1's `(ptr, len, cap)` triple, and cell 2's classifier
    // would route the rebind through REPLAY → load stale data.
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured("let s: String = \"alpha\";");
    assert!(r.errors.is_empty(), "cell 1: {:?}", r.errors);
    let r = s.evaluate_cell_captured("let s: String = \"omega\"; println(s);");
    assert!(r.errors.is_empty(), "cell 2: {:?}", r.errors);
    assert_eq!(
        r.stdout.trim(),
        "omega",
        "cross-cell String shadow must re-capture, not replay; stdout: {:?}",
        r.stdout,
    );
}

#[test]
fn repl_jit_string_mut_let_falls_through_to_passthrough() {
    // Slice c-repl.B.5.2 — `let mut s: String = …` must NOT take the
    // snapshot path. The classifier filters out mut String bindings
    // because capture's cap-zero suppression would leave a same-cell
    // `s.push_str(…)` reading cap=0, reallocating into a fresh
    // buffer, and dropping the global's reference — cell N+1's
    // replay would then load the pre-push buffer and diverge from
    // the interpreter's post-mutation snapshot semantic. Pass-
    // through gives correct (re-evaluating, slower) behavior. We
    // exercise the same-cell mutation to confirm push_str works
    // cleanly without divergence.
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured(
        "let mut s: String = String.new(); s.push_str(\"hi\"); println(s);",
    );
    assert!(
        r.errors.is_empty(),
        "mut String cell should run cleanly: {:?}",
        r.errors,
    );
    assert_eq!(r.stdout.trim(), "hi");
}

#[test]
fn repl_jit_snapshot_covers_f64_bool_char() {
    // Slice c-repl.B.5.1 — verify the snapshot replay path handles
    // every supported primitive kind. Each `tag` fn fires a side-
    // effect on first eval; the replay cell should print only the
    // cached value, not the tag.
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured(
        "fn fnote() -> f64 { println(\"fcalled\"); 3.5 } \
         fn bnote() -> bool { println(\"bcalled\"); true } \
         fn cnote() -> char { println(\"ccalled\"); 'k' }",
    );
    assert!(r.errors.is_empty(), "items: {:?}", r.errors);
    let r = s.evaluate_cell_captured("let f = fnote(); let b = bnote(); let c = cnote();");
    assert!(r.errors.is_empty(), "bind cell: {:?}", r.errors);
    let stdout = r.stdout.trim();
    assert!(
        stdout.contains("fcalled") && stdout.contains("bcalled") && stdout.contains("ccalled"),
        "bind cell should print all three side effects, got: {:?}",
        stdout,
    );
    // Replay cell: every RHS must be skipped, so none of the tag
    // strings should fire. Printing each value confirms the global
    // load delivered the captured datum (not the zero initializer).
    let r = s.evaluate_cell_captured("println(f); println(b); println(c);");
    assert!(r.errors.is_empty(), "use cell: {:?}", r.errors);
    let stdout = r.stdout.trim();
    assert!(
        !stdout.contains("fcalled") && !stdout.contains("bcalled") && !stdout.contains("ccalled"),
        "replay should skip every RHS; stdout: {:?}",
        stdout,
    );
    // Kāra's `println` on a `char` value prints the Unicode codepoint
    // as an integer (107 == 'k'), not the glyph. The captured-value
    // assertion checks the codepoint.
    assert!(
        stdout.contains("3.5") && stdout.contains("true") && stdout.contains("107"),
        "replay should bind each name to its captured value; stdout: {:?}",
        stdout,
    );
}

#[test]
fn repl_jit_cross_cell_shadow_clears_snapshot() {
    // Hypothesis: B.5.1's snapshot survives a cross-cell shadow even
    // though prune_shadowed_lets explicitly clears `let_snapshots` for
    // the interpreter path. Mechanism: `jit_snapshotted_lets` is NOT
    // touched by the prune, so cell 2's `let x = 99` is classified as
    // REPLAY by `compute_snapshot_sets_for_cell` and the codegen path
    // loads from `@__karac_repl_snapshot_x` (still 7) instead of
    // evaluating the new RHS.
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured("let x = 7;");
    assert!(r.errors.is_empty(), "cell 1: {:?}", r.errors);
    let r = s.evaluate_cell_captured("let x = 99; println(x);");
    assert!(r.errors.is_empty(), "cell 2: {:?}", r.errors);
    assert_eq!(
        r.stdout.trim(),
        "99",
        "cross-cell shadow must re-capture, not replay; stdout: {:?}",
        r.stdout,
    );
}

#[test]
#[ignore = "B.5.3 — Vec snapshot port pending; today: cell 2 stdout is \"called2\" (RHS re-evaluated), want \"2\""]
fn repl_jit_vec_let_rhs_is_not_re_evaluated() {
    // Slice c-repl.B.5.3 friction probe — same shape as B.5.1's
    // `repl_jit_let_rhs_is_not_re_evaluated` and B.5.2's String
    // counterpart, but for a `Vec[i64]`-bound let. Cell 1 binds
    // `let xs = make_vec();` where `make_vec()` prints "called" and
    // returns a freshly-allocated Vec; cell 2 references `xs`. The
    // interpreter caches the bound value (its `let_snapshots` map
    // holds the Vec), so cell 2 must NOT re-run `make_vec()`. Today
    // the JIT path lacks Vec/Map snapshot support, so the synthetic
    // source re-emits the let into cell 2's main and `make_vec()`
    // fires again — "called" prints twice across the two cells.
    //
    // Surfaced 2026-05-30: friction confirmed empirically. Expected
    // to pass once B.5.3 lands (Vec snapshot port). Removing the
    // `#[ignore]` is the single trigger that flips this from
    // friction-pin to regression-test.
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured(
        "fn make_vec() -> Vec[i64] { \
            println(\"called\"); \
            let v: Vec[i64] = Vec.new(); \
            v.push(1); v.push(2); \
            v \
         }",
    );
    assert!(r.errors.is_empty(), "fn def: {:?}", r.errors);
    let r = s.evaluate_cell_captured("let xs: Vec[i64] = make_vec();");
    assert!(r.errors.is_empty(), "let cell: {:?}", r.errors);
    assert_eq!(
        r.stdout.trim(),
        "called",
        "let cell should print the side effect once",
    );
    let r = s.evaluate_cell_captured("println(xs.len() as i64);");
    assert!(r.errors.is_empty(), "use cell: {:?}", r.errors);
    assert_eq!(
        r.stdout.trim(),
        "2",
        "use cell should print only `xs.len()` — `make_vec()` must NOT re-run",
    );
}

#[test]
#[ignore = "B.5.3 — Map snapshot port pending; today: cell 2 stdout is \"called-1\" (RHS re-evaluated; the -1 is a separate suspected cross-cell-fn-returning-Map bug, see B.5.3 tracker entry), want \"100\""]
fn repl_jit_map_let_rhs_is_not_re_evaluated() {
    // Slice c-repl.B.5.3 friction probe (Map variant). Same pattern
    // as the Vec probe above; the interpreter caches the bound Map
    // value across cells, the JIT path re-evaluates the RHS until
    // B.5.3 lands.
    //
    // Surfaced 2026-05-30: cell 2 today prints "called-1" rather
    // than the expected "called100" — i.e. `make_map()` re-fires
    // (confirming the B.5.3 gap) AND the re-evaluated `mp` somehow
    // loses its inserted entry (cell 2's `mp.get(1)` returns None,
    // hitting the `-1` arm). Standalone non-REPL JIT of the same
    // `make_map()` pattern prints 100 correctly, so the
    // entry-loss is a REPL-cell-mode-only divergence — likely tied
    // to the cross-cell declare-only path for `make_map` interacting
    // with Map's heap-backed storage somehow. Once B.5.3 lands,
    // cell 2 won't re-call `make_map()` (it'll load the captured
    // Map handle from the snapshot global), so the entry-loss bug
    // becomes moot for this assertion — but the bug itself is
    // tracked under B.5.3 because it's a pre-existing hazard for
    // any future REPL workload that does mutate a Map across cells.
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured(
        "fn make_map() -> Map[i64, i64] { \
            println(\"called\"); \
            let m: Map[i64, i64] = Map.new(); \
            m.insert(1, 100); \
            m \
         }",
    );
    assert!(r.errors.is_empty(), "fn def: {:?}", r.errors);
    let r = s.evaluate_cell_captured("let mp: Map[i64, i64] = make_map();");
    assert!(r.errors.is_empty(), "let cell: {:?}", r.errors);
    assert_eq!(
        r.stdout.trim(),
        "called",
        "let cell should print the side effect once",
    );
    let r =
        s.evaluate_cell_captured("match mp.get(1) { Some(v) => println(v), None => println(-1), }");
    assert!(r.errors.is_empty(), "use cell: {:?}", r.errors);
    assert_eq!(
        r.stdout.trim(),
        "100",
        "use cell should print only the looked-up value — `make_map()` must NOT re-run",
    );
}

#[test]
fn repl_jit_banner_advertises_jit_mode() {
    // Slice c-repl.B.B — drive the actual `karac repl` binary with
    // `KARAC_REPL_JIT=1`. Verifies the banner picked up the JIT tag
    // so users have a visible signal that the env flag took effect.
    // rustyline drops to a non-TTY fallback when stdin is piped and
    // exits cleanly on EOF — we don't try to send cells through this
    // path (those go through the in-process Session tests above),
    // we only assert the banner string.
    use std::io::Write;
    use std::process::{Command, Stdio};

    let karac = env!("CARGO_BIN_EXE_karac");
    let runner = env!("CARGO_BIN_EXE_karac_jit_runner");

    let mut child = Command::new(karac)
        .arg("repl")
        .env("KARAC_REPL_JIT", "1")
        .env("KARAC_JIT_RUNNER", runner)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn karac repl");
    // Close stdin so rustyline sees EOF and the loop exits.
    {
        let stdin = child.stdin.as_mut().expect("child stdin");
        let _ = stdin.write_all(b"");
    }
    drop(child.stdin.take());
    let out = child.wait_with_output().expect("wait karac repl");
    assert!(out.status.success(), "karac repl exit: {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("JIT"),
        "JIT banner tag missing under KARAC_REPL_JIT=1; stdout: {:?}",
        stdout,
    );
    assert!(
        stdout.contains("Kāra REPL"),
        "banner heading missing; stdout: {:?}",
        stdout,
    );
}

#[test]
fn repl_jit_reset_clears_snapshot_state() {
    // Slice c-repl.B.B — `:reset` under JIT mode must clear
    // `jit_snapshotted_lets` (the in-process map of names → primitive
    // kinds) AND drop the runner client (whose JITDylib holds the
    // matching snapshot globals). Without that clear, a post-reset
    // `let x = …` whose name collides with a pre-reset binding takes
    // the snapshot-replay path against a stale-or-missing global.
    //
    // Scenario:
    //   cell 1: `let x = 7;` — captures 7 into the runner's
    //     @__karac_repl_snapshot_x global; records ("x", I64) in
    //     `jit_snapshotted_lets`.
    //   `:reset` — clears persistent_lets, MUST also clear the JIT
    //     state and drop the client. Next cell respawns a fresh
    //     runner with an empty JITDylib.
    //   cell 2: `let x = 99; println(x);` — must print 99. Without
    //     the fix, codegen sees "x" still in `jit_snapshotted_lets`,
    //     emits a load of @__karac_repl_snapshot_x (now unmapped on
    //     the new runner), and either fails to link or returns
    //     garbage instead of the fresh `99`.
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured("let x = 7;");
    assert!(r.errors.is_empty(), "cell 1: {:?}", r.errors);

    s.reset_persistent_lets();

    let r = s.evaluate_cell_captured("let x = 99; println(x);");
    assert!(
        r.errors.is_empty(),
        "cell after :reset should run cleanly; got errors: {:?}",
        r.errors,
    );
    assert_eq!(
        r.stdout.trim(),
        "99",
        "post-reset `let x = 99` must NOT take the snapshot-replay path; \
         stdout: {:?}",
        r.stdout,
    );
}
