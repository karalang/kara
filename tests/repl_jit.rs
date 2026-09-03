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

#![cfg(feature = "llvm")]

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
fn repl_jit_string_mut_let_carries_in_cell_mutation_across_cells() {
    // `let mut` container bindings used to be EXCLUDED from the snapshot
    // path (the slice-B.5.2/B.5.3 "mut filter"), on the reasoning that
    // capture's cap-zero ownership transfer would break a same-cell
    // mutation that ran after it. That was true only because capture fired
    // at the `let`; it now fires at END OF CELL, after every statement, so
    // the exclusion is gone (B-2026-07-29-20). Its real effect was the bug:
    // with no capture, cell N+1 re-evaluated the RHS and the mutation
    // vanished. Both halves are asserted here — the same-cell mutation
    // still works, AND it survives the cell boundary.
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
    let r2 = s.evaluate_cell_captured("println(s);");
    assert!(
        r2.errors.is_empty(),
        "cell 2 should run cleanly: {:?}",
        r2.errors,
    );
    assert_eq!(
        r2.stdout.trim(),
        "hi",
        "the same-cell push_str must survive the cell boundary (was \"\" \
         when the RHS got re-evaluated)"
    );
}

/// B-2026-08-30-1 — a mutation made in a cell AFTER the declaring one must
/// survive to the next cell.
///
/// `emit_pending_snapshot_captures` (B-2026-07-29-20) moved the capture from
/// the `let` to end of cell, which fixed a mutation made in the SAME cell as
/// the declaration — the shape the four
/// `*_carries_in_cell_mutation_across_cells` tests above pin. It left the
/// replay classification exclusive: once a name had a global,
/// `compute_snapshot_sets_for_cell` routed it to `replay` and `continue`d, so
/// it never entered `capture` and the global was written exactly ONCE, by the
/// declaring cell. Every later cell loaded a value it could not write back.
///
/// Map and Set hid the gap and are kept here as controls: their globals hold
/// a handle POINTER, so an in-place `insert` is visible through a stale
/// global and those two shapes were correct by aliasing. The seven by-value
/// shapes were not — they reverted to the initializer, and the two heap ones
/// did worse than revert (see the two tests below).
#[test]
fn repl_jit_later_cell_mutation_survives_snapshot_replay() {
    // (label, declare cell, mutate cell, read cell, expected stdout)
    let cases: &[(&str, &str, &str, &str, &str)] = &[
        (
            "i64",
            "let mut n: i64 = 0;",
            "n = 5;",
            "println(f\"{n}\");",
            "5",
        ),
        (
            "f64",
            "let mut f: f64 = 0.0;",
            "f = 2.5;",
            "println(f\"{f}\");",
            "2.5",
        ),
        (
            "bool",
            "let mut b: bool = false;",
            "b = true;",
            "println(f\"{b}\");",
            "true",
        ),
        (
            "char",
            "let mut c: char = 'a';",
            "c = 'z';",
            "println(f\"{c}\");",
            "z",
        ),
        (
            "String reassign",
            "let mut s = f\"a\";",
            "s = s + f\"b\";",
            "println(f\"{s}\");",
            "ab",
        ),
        (
            "String push_str",
            "let mut s: String = String.new();",
            "s.push_str(\"hi\");",
            "println(f\"{s}\");",
            "hi",
        ),
        (
            "Vec",
            "let mut v: Vec[i64] = Vec.new();",
            "v.push(7);",
            "println(f\"{v.len()}\");",
            "1",
        ),
        // Controls: correct before the fix (handle-pointer aliasing) and
        // must stay correct after it.
        (
            "Map (control)",
            "let mut m: Map[i64, i64] = Map.new();",
            "m.insert(1, 3);",
            "println(f\"{m.len()}\");",
            "1",
        ),
        (
            "Set (control)",
            "let mut q: Set[i64] = Set.new();",
            "q.insert(9);",
            "println(f\"{q.len()}\");",
            "1",
        ),
    ];

    for (label, declare, mutate, read, want) in cases {
        let mut s = Session::new();
        enable_jit(&mut s);
        for (idx, src) in [declare, mutate].iter().enumerate() {
            let r = s.evaluate_cell_captured(src);
            assert!(
                r.errors.is_empty(),
                "[{label}] cell {} should run cleanly: {:?}",
                idx + 1,
                r.errors,
            );
        }
        let r = s.evaluate_cell_captured(read);
        assert!(
            r.errors.is_empty(),
            "[{label}] read cell should run cleanly: {:?}",
            r.errors,
        );
        assert_eq!(
            r.stdout.trim(),
            *want,
            "[{label}] the cell-2 mutation must survive to cell 3 (the \
             snapshot global was written once, by cell 1, and never \
             refreshed)",
        );
    }
}

/// B-2026-08-30-1, the half that is worse than staleness: a later-cell Vec
/// growth left the snapshot global pointing at a FREED buffer.
///
/// Cell 1 fills exactly to `cap`, so cell 2's pushes realloc and free the
/// original allocation. With no write-back the global still held the old
/// `{ptr, len, cap}`, so cell 3 read freed heap: it reported `len=4` and
/// `v[0]` came back as a raw pointer value (94542648599680 on the measuring
/// run), not 1. Asserting the ELEMENTS, not just the length, is what
/// distinguishes reading freed memory from merely reading a stale length.
#[test]
fn repl_jit_later_cell_vec_growth_does_not_strand_the_snapshot_global() {
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured(
        "let mut v: Vec[i64] = Vec.new(); v.push(1); v.push(2); v.push(3); v.push(4);",
    );
    assert!(r.errors.is_empty(), "declare cell: {:?}", r.errors);
    let r = s.evaluate_cell_captured("v.push(5); v.push(6); v.push(7); v.push(8); v.push(9);");
    assert!(r.errors.is_empty(), "growth cell: {:?}", r.errors);
    let r = s.evaluate_cell_captured("println(f\"{v.len()}|{v[0]}|{v[8]}\");");
    assert!(r.errors.is_empty(), "read cell: {:?}", r.errors);
    assert_eq!(
        r.stdout.trim(),
        "9|1|9",
        "cell 3 must see the grown buffer, not the freed one",
    );
}

/// B-2026-08-30-1, the same freed-buffer shape for String: a later-cell
/// reassignment frees the buffer the global points at.
///
/// Pre-fix cell 3 printed `len=10` (the stale length) with the bytes
/// `A\0\0\0\0\0\0\0\xf3\x0f` — freed heap, not `abcdefghij`. The length is
/// asserted alongside the text so a regression that merely truncates is not
/// mistaken for this one.
#[test]
fn repl_jit_later_cell_string_reassign_does_not_strand_the_snapshot_global() {
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured("let mut s = f\"abcdefghij\";");
    assert!(r.errors.is_empty(), "declare cell: {:?}", r.errors);
    let r = s.evaluate_cell_captured("s = s + f\"KLM\";");
    assert!(r.errors.is_empty(), "reassign cell: {:?}", r.errors);
    let r = s.evaluate_cell_captured("println(f\"{s.len()}|{s}\");");
    assert!(r.errors.is_empty(), "read cell: {:?}", r.errors);
    assert_eq!(
        r.stdout.trim(),
        "13|abcdefghijKLM",
        "cell 3 must see the reassigned string, not the freed buffer",
    );
}

/// B-2026-08-30-1 — the write-back must chain: a value mutated in cell 2 and
/// again in cell 3 has to carry BOTH mutations into cell 4. One-shot capture
/// and once-per-cell capture are indistinguishable on a single mutation, so
/// this is the case that pins the write-back as repeating rather than moved.
#[test]
fn repl_jit_snapshot_write_back_chains_across_cells() {
    let mut s = Session::new();
    enable_jit(&mut s);
    for src in [
        "let mut n: i64 = 1; let mut t = f\"a\";",
        "n = n + 1; t = t + f\"b\";",
        "n = n * 10; t = t + f\"c\";",
    ] {
        let r = s.evaluate_cell_captured(src);
        assert!(r.errors.is_empty(), "cell {src:?}: {:?}", r.errors);
    }
    let r = s.evaluate_cell_captured("println(f\"{n}|{t}\");");
    assert!(r.errors.is_empty(), "read cell: {:?}", r.errors);
    assert_eq!(
        r.stdout.trim(),
        "20|abc",
        "both cell-2 and cell-3 mutations must reach cell 4",
    );
}

/// B-2026-08-30-7 — a binding the JIT lane cannot snapshot must SAY so at the
/// cell that declares it.
///
/// The pass-through path rebuilds such a binding by re-running its initializer
/// in every later cell, so a mutation is reverted and a side-effecting
/// initializer re-executes — both silently, and both divergences from
/// `--interp`, which carries every type. Until the snapshot tier covers these
/// types (B.5.3d), the guarantee this test pins is that the divergence is
/// ANNOUNCED rather than silent.
///
/// The note fires at the DECLARING cell and only there: from the next cell on,
/// the name lives in `persistent_lets` and the walk skips it.
#[test]
fn repl_jit_passthrough_binding_warns_at_the_declaring_cell() {
    let mut s = Session::new();
    enable_jit(&mut s);
    // The subject has been migrated TWICE — `Vec[String]`, then
    // `Vec[Vec[i64]]` — because each widening of the tier moved the previous
    // choice into it and turned this test red. So it is now a shape the tier
    // will never admit rather than one it has not reached yet: a `Vec` of a
    // type with a user `impl Drop`. That exclusion is structural, not a
    // deferral — admitting it would hand the value to a global nobody tears
    // down, so the destructor would never run. What this test pins is the
    // note's CONTENT and its once-per-binding firing, and picking a
    // permanently-excluded subject is what stops it chasing the frontier.
    let r = s.evaluate_cell_captured(
        "struct D { n: i64 }\nimpl Drop for D { fn drop(mut ref self) { println(f\"dD\"); } }\nfn mkd() -> D { D { n: 1 } }",
    );
    assert!(r.errors.is_empty(), "items cell: {:?}", r.errors);
    let r = s.evaluate_cell_captured("let mut v: Vec[D] = Vec.new();");
    assert!(r.errors.is_empty(), "declare cell: {:?}", r.errors);
    let note = r
        .notes
        .iter()
        .find(|n| n.contains("repl-jit-no-snapshot"))
        .unwrap_or_else(|| panic!("expected a no-snapshot note; notes: {:?}", r.notes));
    assert!(
        note.contains("`v`") && note.contains("Vec[D]"),
        "the note must name the binding and its type; got: {note}",
    );
    assert!(
        note.contains("--interp"),
        "the note must name the lane that is correct; got: {note}",
    );

    // Second cell mutates it. No repeat — the note is a property of the
    // binding, not of every cell that touches it.
    // Through a helper rather than `v.push(D { n: 1 });`: a struct literal as
    // a call argument, alone in a cell, resolves as `undefined name 'v'` on
    // BOTH backends — an unrelated REPL cell-classification bug, filed as
    // B-2026-09-01-37.
    let r2 = s.evaluate_cell_captured("v.push(mkd());");
    assert!(r2.errors.is_empty(), "mutate cell: {:?}", r2.errors);
    assert!(
        !r2.notes.iter().any(|n| n.contains("repl-jit-no-snapshot")),
        "the note must fire once, at the declaring cell; got: {:?}",
        r2.notes,
    );
}

/// B-2026-08-30-7, second half: an immutable binding whose initializer CALLS a
/// session-defined function also diverges, because the call re-runs on every
/// later cell — B.5.1's own `let log = read_file("big.json")` example. Pinned
/// separately from the `let mut` case because it reaches the note by the other
/// condition, and a regression could plausibly break one and not the other.
#[test]
fn repl_jit_passthrough_side_effecting_initializer_warns() {
    let mut s = Session::new();
    enable_jit(&mut s);
    // Same subject migration as the test above, and for the same reason: a
    // permanently-excluded shape rather than a not-yet-reached one.
    let r = s.evaluate_cell_captured(
        "struct D { n: i64 }\nimpl Drop for D { fn drop(mut ref self) { println(f\"dD\"); } }\nfn mk() -> Vec[D] { Vec.new() }",
    );
    assert!(r.errors.is_empty(), "items cell: {:?}", r.errors);
    let r = s.evaluate_cell_captured("let cfg = mk();");
    assert!(r.errors.is_empty(), "declare cell: {:?}", r.errors);
    assert!(
        r.notes
            .iter()
            .any(|n| n.contains("repl-jit-no-snapshot") && n.contains("`cfg`")),
        "an immutable binding initialized by a session fn call must warn; notes: {:?}",
        r.notes,
    );
}

/// B-2026-08-30-7 control — a binding the snapshot tier DOES cover must stay
/// quiet. Without this the note could regress into firing on everything, which
/// would bury the cases that matter behind noise on every ordinary cell.
#[test]
fn repl_jit_eligible_binding_emits_no_passthrough_warning() {
    let mut s = Session::new();
    enable_jit(&mut s);
    for src in [
        "let mut n: i64 = 0;",
        "let mut t = f\"a\";",
        "let mut v: Vec[i64] = Vec.new();",
        "let mut m: Map[i64, i64] = Map.new();",
        "let mut q: Set[i64] = Set.new();",
    ] {
        let r = s.evaluate_cell_captured(src);
        assert!(r.errors.is_empty(), "{src}: {:?}", r.errors);
        assert!(
            !r.notes.iter().any(|n| n.contains("repl-jit-no-snapshot")),
            "{src} is snapshot-eligible and must not warn; notes: {:?}",
            r.notes,
        );
    }
}

/// B-2026-08-30-7 control — an IMMUTABLE binding with a constructor-shaped
/// initializer is on the pass-through path too, but re-running the initializer
/// is observationally identical, so there is nothing to report. This is the
/// common shape in an interactive session; warning here would make the note
/// worthless.
///
/// The struct carries a `String` field deliberately: a struct of pure scalars
/// is snapshot-eligible now (the by-value tier below), so it would pass this
/// test for the wrong reason — quiet because it is COVERED rather than quiet
/// because the pass-through is harmless. A heap-carrying field keeps it on the
/// path the test is about.
#[test]
fn repl_jit_immutable_constructor_binding_emits_no_passthrough_warning() {
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured("struct P { x: String }");
    assert!(r.errors.is_empty(), "items cell: {:?}", r.errors);
    let r = s.evaluate_cell_captured("let p = P { x: f\"a\" };");
    assert!(r.errors.is_empty(), "declare cell: {:?}", r.errors);
    assert!(
        !r.notes.iter().any(|n| n.contains("repl-jit-no-snapshot")),
        "a pure immutable binding must not warn; notes: {:?}",
        r.notes,
    );
}

/// B-2026-08-30-7, the DIVERGENCE half — a mutation to a by-value binding made
/// in a later cell must survive the cell boundary on the JIT lane.
///
/// The row's own measurement is the `Option[i64]` row of its table: `o =
/// Option.Some(4)` in cell 2 read back `false` under `karac repl` and `true`
/// under `karac repl --interp`, because `Option` was not snapshot-eligible and
/// the pass-through re-ran `Option.None` in every later cell. The other four
/// shapes are the rest of the by-value class — a narrow scalar (the width the
/// original tier deliberately excluded), a tuple, a user struct, and a user
/// enum.
///
/// Each is asserted against the value the interpreter produces, which is the
/// standard this row is measured against throughout.
#[test]
fn repl_jit_by_value_binding_carries_a_later_cell_mutation() {
    for (items, decl, mutate, read, want) in [
        (
            "",
            "let mut o: Option[i64] = Option.None;",
            "o = Option.Some(4);",
            "println(f\"{o.is_some()}\");",
            "true",
        ),
        (
            "",
            "let mut n: i32 = 0;",
            "n = 5;",
            "println(f\"{n}\");",
            "5",
        ),
        (
            "",
            "let mut t: (i64, bool) = (1, false);",
            "t = (7, true);",
            "println(f\"{t.0} {t.1}\");",
            "7 true",
        ),
        (
            "struct P { x: i64, y: i64 }",
            "let mut p: P = P { x: 1, y: 2 };",
            "p.x = 9;",
            "println(f\"{p.x} {p.y}\");",
            "9 2",
        ),
        (
            "enum E { A, B(i64) }",
            "let mut e: E = E.A;",
            "e = E.B(3);",
            "println(f\"{match e { E.A => 0, E.B(n) => n }}\");",
            "3",
        ),
    ] {
        let mut s = Session::new();
        enable_jit(&mut s);
        if !items.is_empty() {
            let r = s.evaluate_cell_captured(items);
            assert!(r.errors.is_empty(), "{items}: {:?}", r.errors);
        }
        let r = s.evaluate_cell_captured(decl);
        assert!(r.errors.is_empty(), "{decl}: {:?}", r.errors);
        assert!(
            !r.notes.iter().any(|n| n.contains("repl-jit-no-snapshot")),
            "{decl} is by-value and must reach the snapshot tier; notes: {:?}",
            r.notes,
        );
        let r = s.evaluate_cell_captured(mutate);
        assert!(r.errors.is_empty(), "{mutate}: {:?}", r.errors);
        let r = s.evaluate_cell_captured(read);
        assert!(r.errors.is_empty(), "{read}: {:?}", r.errors);
        assert_eq!(
            r.stdout.trim(),
            want,
            "a mutation made in the cell AFTER the declaring one must survive \
             the boundary for `{decl}`; stdout: {:?}",
            r.stdout,
        );
    }
}

/// B-2026-08-30-7, the SIDE-EFFECT half — a by-value binding whose initializer
/// prints must print once across the session, not once per later cell.
///
/// This is the second divergence the row measures, and it is a different
/// mechanism from the one above: the mutation loss comes from cell N+1
/// rebuilding the binding, the re-execution from that rebuild running the RHS.
/// A fix that replayed the value but still emitted the RHS would pass the test
/// above and fail this one.
#[test]
fn repl_jit_by_value_initializer_runs_once_across_cells() {
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured(
        "fn mk() -> Option[i64] { println(f\"SIDE EFFECT\"); Option.Some(1) }",
    );
    assert!(r.errors.is_empty(), "items: {:?}", r.errors);
    let r = s.evaluate_cell_captured("let mut o: Option[i64] = mk();");
    assert!(r.errors.is_empty(), "declare: {:?}", r.errors);
    assert_eq!(
        r.stdout.matches("SIDE EFFECT").count(),
        1,
        "the declaring cell runs the initializer exactly once; stdout: {:?}",
        r.stdout,
    );
    for cell in ["println(f\"two\");", "println(f\"three\");"] {
        let r = s.evaluate_cell_captured(cell);
        assert!(r.errors.is_empty(), "{cell}: {:?}", r.errors);
        assert_eq!(
            r.stdout.matches("SIDE EFFECT").count(),
            0,
            "a later cell must not re-run the initializer; stdout: {:?}",
            r.stdout,
        );
    }
}

/// B-2026-08-30-7 — the by-value tier must NOT admit a type whose declared
/// shape is a lie about its storage.
///
/// `DataFrame` walks as a struct of by-value fields, so a field-shape-only
/// eligibility rule accepts it — and then codegen lowers it to a bare `ptr`
/// whose pointee the slot's scope-exit cleanup frees, the global keeps the
/// freed pointer, and the next cell loads a dangling handle. Measured while
/// building this tier: the runner died mid-cell where `--interp` printed `0`.
/// The guard is that a named type must be declared in the session's own
/// source; this pins that `DataFrame` is not, and stays on pass-through.
#[test]
fn repl_jit_by_value_tier_excludes_an_opaque_handle_type() {
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured("let mut d: DataFrame = DataFrame.new();");
    assert!(r.errors.is_empty(), "declare: {:?}", r.errors);
    assert!(
        r.notes
            .iter()
            .any(|n| n.contains("repl-jit-no-snapshot") && n.contains("`d`")),
        "an opaque-handle type must stay on pass-through; notes: {:?}",
        r.notes,
    );
    let r = s.evaluate_cell_captured("println(f\"{d.height()}\");");
    assert!(r.errors.is_empty(), "use: {:?}", r.errors);
    assert_eq!(
        r.stdout.trim(),
        "0",
        "the handle must still be live in a later cell; stdout: {:?}",
        r.stdout,
    );
}

/// B-2026-08-30-7 — a struct carrying a user `impl Drop` must stay on
/// pass-through.
///
/// Its fields are by-value, so the shape walk alone would admit it. But
/// capture hands the value to a global nobody tears down, so the destructor
/// would never run — trading this row's divergence for a different one. The
/// exclusion is by name: any type with a `Drop` impl in the session.
#[test]
fn repl_jit_by_value_tier_excludes_a_drop_carrying_struct() {
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured(
        "struct D { x: i64 } \
         impl Drop for D { fn drop(mut ref self) { println(f\"dD\"); } }",
    );
    assert!(r.errors.is_empty(), "items: {:?}", r.errors);
    let r = s.evaluate_cell_captured("let mut d: D = D { x: 1 };");
    assert!(r.errors.is_empty(), "declare: {:?}", r.errors);
    assert!(
        r.notes
            .iter()
            .any(|n| n.contains("repl-jit-no-snapshot") && n.contains("`d`")),
        "a Drop-carrying struct must stay on pass-through; notes: {:?}",
        r.notes,
    );
}

/// B-2026-08-30-7, the HEAP-ELEMENT half of the container tier —
/// `Vec[String]`, `Map[String, V]`, `Map[K, String]` and `Set[String]` must
/// carry a mutation made in a LATER cell, exactly as their primitive-element
/// siblings already do.
///
/// `Vec[String]` and `Map[String, i64]` are the row's own two headline
/// measurements: both read back their initializer on the JIT lane while
/// `--interp` answered correctly, because an element type outside the
/// four primitives fell off `snapshot_kind_for_type` into pass-through and
/// the next cell rebuilt the binding from its RHS.
///
/// The deferral's stated reason was that these shapes "need per-element drop
/// accounting the shallow handle transfer can't carry". That is the reason
/// FREEING would need it. Capture does not free: it suppresses the slot's
/// cleanup WHOLESALE — `cap = 0` for Vec, and for Map/Set by retracting the
/// queued `FreeMapHandle` — and the drain's entire per-element walk sits
/// inside the branch that suppression turns off. So the transfer covers the
/// whole tree, elements included, with no second owner to reconcile; the
/// buffers outlive the cell and are reclaimed when the JITDylib is torn
/// down, which is the policy `String` and `Vec[i64]` have always followed.
///
/// Each row also asserts NO `repl-jit-no-snapshot` note fires: the note keys
/// on `snapshot_kind_for_type` returning `None`, so it must narrow exactly as
/// the tier widens. A fix that snapshotted the value but left the warning
/// standing would be telling the user to switch lanes for no reason.
#[test]
fn repl_jit_heap_element_container_carries_a_later_cell_mutation() {
    for (decl, mutate, read, want) in [
        (
            "let mut v: Vec[String] = Vec.new();",
            "v.push(f\"a\");",
            "println(f\"{v.len()}\");",
            "1",
        ),
        // Reads the ELEMENT back, not just the length — the length alone
        // would pass on a snapshot that transferred the outer triple and
        // lost the element buffers.
        (
            "let mut v2: Vec[String] = Vec.new();",
            "v2.push(f\"hi\");",
            "println(v2[0]);",
            "hi",
        ),
        (
            "let mut m: Map[String, i64] = Map.new();",
            "m.insert(f\"k\", 7);",
            "println(f\"{m.len()}\");",
            "1",
        ),
        // A String on the VALUE side as well as the key, so the widening is
        // pinned on both halves of `kind_for` rather than only the first.
        (
            "let mut m2: Map[String, String] = Map.new();",
            "m2.insert(f\"k\", f\"v\");",
            "let g = m2.get(f\"k\"); println(f\"{g}\");",
            "Some(v)",
        ),
        (
            "let mut st: Set[String] = Set.new();",
            "st.insert(f\"x\");",
            "println(f\"{st.len()}\");",
            "1",
        ),
    ] {
        let mut s = Session::new();
        enable_jit(&mut s);
        let r = s.evaluate_cell_captured(decl);
        assert!(r.errors.is_empty(), "{decl}: {:?}", r.errors);
        assert!(
            !r.notes.iter().any(|n| n.contains("repl-jit-no-snapshot")),
            "{decl} reaches the snapshot tier, so the pass-through note must \
             not fire; notes: {:?}",
            r.notes,
        );
        let r = s.evaluate_cell_captured(mutate);
        assert!(r.errors.is_empty(), "{mutate}: {:?}", r.errors);
        let r = s.evaluate_cell_captured(read);
        assert!(r.errors.is_empty(), "{read}: {:?}", r.errors);
        assert_eq!(
            r.stdout.trim(),
            want,
            "a mutation made in the cell AFTER the declaring one must survive \
             the boundary for `{decl}`; stdout: {:?}",
            r.stdout,
        );
    }
}

/// B-2026-08-30-7, the SIDE-EFFECT half for a heap-element container.
///
/// Same distinction the by-value pair draws: the mutation loss comes from
/// cell N+1 rebuilding the binding, the re-execution from that rebuild
/// running the RHS. A fix that replayed the value but still emitted the RHS
/// passes the test above and fails this one. Measured pre-fix at three
/// executions across four cells, against `--interp`'s one.
#[test]
fn repl_jit_heap_element_container_initializer_runs_once_across_cells() {
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s
        .evaluate_cell_captured("fn mkv() -> Vec[String] { println(f\"SIDE EFFECT\"); Vec.new() }");
    assert!(r.errors.is_empty(), "items: {:?}", r.errors);
    let r = s.evaluate_cell_captured("let mut v: Vec[String] = mkv();");
    assert!(r.errors.is_empty(), "declare: {:?}", r.errors);
    assert_eq!(
        r.stdout.matches("SIDE EFFECT").count(),
        1,
        "the declaring cell runs the initializer exactly once; stdout: {:?}",
        r.stdout,
    );
    for cell in ["println(f\"two\");", "println(f\"three\");"] {
        let r = s.evaluate_cell_captured(cell);
        assert!(r.errors.is_empty(), "{cell}: {:?}", r.errors);
        assert_eq!(
            r.stdout.matches("SIDE EFFECT").count(),
            0,
            "a later cell must not re-run the initializer; stdout: {:?}",
            r.stdout,
        );
    }
}

/// B-2026-08-30-7 — a snapshotted `Map`/`Set` whose initializer is NOT a
/// literal `Map.new()` / `Set.new()` must still survive the cell boundary.
///
/// Map/Set cleanup is queue-driven rather than sentinel-driven, and its
/// snapshot suppression used to live at the two CONSTRUCTOR functions
/// (`compile_map_new_stmt` / `compile_set_new_stmt` skip `track_map_var` for
/// a captured name). That keyed the suppression on the initializer's SHAPE,
/// and only one shape reaches those functions: a map returned from a session
/// function was tracked like an ordinary local, so the handle the snapshot
/// global pointed at was freed at end of cell and the next cell loaded a
/// dangling pointer — the JIT runner died mid-cell where `--interp` printed
/// the map's size.
///
/// The `Map[i64, i64]` row is deliberately included and deliberately first:
/// that shape was ALREADY in the tier before this row widened it, so the
/// hole predates the widening rather than arriving with it, and this pins
/// the pre-existing half against regression too. Suppression now happens at
/// capture, keyed on the fact that the handle has just been handed to a
/// global, which covers every initializer shape at once.
#[test]
fn repl_jit_snapshotted_map_from_a_function_survives_the_cell_boundary() {
    for (items, decl, mutate, read, want) in [
        (
            "fn mkm() -> Map[i64, i64] { let mut m: Map[i64, i64] = Map.new(); \
             m.insert(1, 1); m }",
            "let mut m: Map[i64, i64] = mkm();",
            "m.insert(2, 2);",
            "println(f\"{m.len()}\");",
            "2",
        ),
        (
            "fn mkms() -> Map[String, i64] { let mut m: Map[String, i64] = Map.new(); \
             m.insert(f\"a\", 1); m }",
            "let mut ms: Map[String, i64] = mkms();",
            "ms.insert(f\"b\", 2);",
            "let g = ms.get(f\"a\"); println(f\"{g}\");",
            "Some(1)",
        ),
        (
            "fn mkss() -> Set[String] { let mut s: Set[String] = Set.new(); \
             s.insert(f\"a\"); s }",
            "let mut ss: Set[String] = mkss();",
            "ss.insert(f\"b\");",
            "println(f\"{ss.len()}\");",
            "2",
        ),
    ] {
        let mut s = Session::new();
        enable_jit(&mut s);
        let r = s.evaluate_cell_captured(items);
        assert!(r.errors.is_empty(), "{items}: {:?}", r.errors);
        let r = s.evaluate_cell_captured(decl);
        assert!(r.errors.is_empty(), "{decl}: {:?}", r.errors);
        let r = s.evaluate_cell_captured(mutate);
        assert!(r.errors.is_empty(), "{mutate}: {:?}", r.errors);
        let r = s.evaluate_cell_captured(read);
        assert!(r.errors.is_empty(), "{read}: {:?}", r.errors);
        assert_eq!(
            r.stdout.trim(),
            want,
            "a handle handed to the snapshot global must not be freed at end \
             of cell for `{decl}`; stdout: {:?}",
            r.stdout,
        );
    }
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
    // This assertion used to read `107` — the codepoint — under a comment
    // stating that "Kāra's `println` on a `char` value prints the Unicode
    // codepoint as an integer, not the glyph". That was never the language's
    // behaviour: `println('k')` prints `k`, and so does the interpreter here.
    // What the author had actually hit was B-2026-08-26-20 — a `char` whose
    // producing form had no arm in codegen's `expr_is_char` allowlist rendered
    // as its integer — and the REPL's global-load replay is one such form. The
    // defect got written down as the specification, so the test passed while
    // pinning the wrong answer, and would have blocked the fix.
    assert!(
        stdout.contains("3.5") && stdout.contains("true") && stdout.contains('k'),
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
            let mut v: Vec[i64] = Vec.new(); \
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
fn repl_jit_vec_cross_cell_shadow_drops_runner() {
    // Slice c-repl.B.5.3 — Vec entries land in `jit_snapshotted_lets`
    // the same way primitive/String entries do, so the cross-cell
    // shadow detection in `prune_shadowed_lets` (B.5.1 follow-up)
    // picks them up uniformly. Cell 1 binds a Vec[i64]; cell 2
    // rebinds the same name to a different Vec without `:reset`.
    // The shadow detection drops the runner, the fresh runner re-
    // captures cell 2's new value, and the use cell prints the new
    // length — NOT the stale cell-1 buffer's length.
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured("let mut xs: Vec[i64] = Vec.new(); xs.push(1); xs.push(2);");
    assert!(r.errors.is_empty(), "cell 1: {:?}", r.errors);
    let r = s.evaluate_cell_captured(
        "let mut xs: Vec[i64] = Vec.new(); xs.push(10); xs.push(20); xs.push(30); println(xs.len() as i64);",
    );
    assert!(r.errors.is_empty(), "cell 2: {:?}", r.errors);
    assert_eq!(
        r.stdout.trim(),
        "3",
        "cross-cell Vec shadow must re-capture, not replay; stdout: {:?}",
        r.stdout,
    );
}

#[test]
fn repl_jit_vec_mut_let_carries_in_cell_mutation_across_cells() {
    // `let mut` container bindings used to be EXCLUDED from the snapshot
    // path (the slice-B.5.2/B.5.3 "mut filter"), on the reasoning that
    // capture's cap-zero ownership transfer would break a same-cell
    // mutation that ran after it. That was true only because capture fired
    // at the `let`; it now fires at END OF CELL, after every statement, so
    // the exclusion is gone (B-2026-07-29-20). Its real effect was the bug:
    // with no capture, cell N+1 re-evaluated the RHS and the mutation
    // vanished. Both halves are asserted here — the same-cell mutation
    // still works, AND it survives the cell boundary.
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured(
        "let mut xs: Vec[i64] = Vec.new(); xs.push(7); xs.push(8); println(xs.len() as i64);",
    );
    assert!(
        r.errors.is_empty(),
        "mut Vec cell should run cleanly: {:?}",
        r.errors,
    );
    assert_eq!(r.stdout.trim(), "2");
    let r2 = s.evaluate_cell_captured("println(xs.len() as i64); println(xs[0]);");
    assert!(
        r2.errors.is_empty(),
        "cell 2 should run cleanly: {:?}",
        r2.errors,
    );
    assert_eq!(
        r2.stdout.trim(),
        "2\n7",
        "both same-cell pushes must survive the cell boundary (was 0 \
         when the RHS got re-evaluated)"
    );
}

#[test]
fn repl_jit_map_let_rhs_is_not_re_evaluated() {
    // Slice c-repl.B.5.3b — Map snapshot port. Cell 1 binds a Map
    // via `Map.new()` and inserts an entry in the same cell. Cell 2
    // reads the entry via `m.get(1)`. The persistent-let replay
    // mechanism re-emits `let m = Map.new();` into cell 2's synth
    // source (the insert / println in cell 1's body don't persist
    // across cells — only top-level lets do). Pre-B.5.3b the JIT
    // path re-evaluated the let RHS in cell 2, producing a fresh
    // empty Map → `get(1)` returns None → prints -1. Post-B.5.3b
    // the snapshot mechanism replays from a global holding cell 1's
    // populated Map handle → `get(1)` returns Some(100) → prints 100.
    //
    // Side-effect detection differs from the Vec / String / primitive
    // probes (those rely on a `println("called")` in a fn body that
    // returns a populated heap container). Map's fn-return path has
    // a pre-existing codegen bug — `suppress_cleanup_for_tail_return`
    // suppresses Vec/String track cleanup on tail-return Identifier
    // expressions but NOT Map's `FreeMapHandle`, so a Map returned
    // from a fn that allocated it via `Map.new()` gets freed at the
    // fn's scope exit before the caller receives the handle. AOT
    // happens to print correctly because LLVM's post-codegen O2
    // passes elide the dead store-free; JIT runs pre-O2 IR. The
    // tail-return suppression for Map is a separate codegen slice
    // (filed under "Map tail-return cleanup suppression"); this test
    // sidesteps it by using `Map.new()` in the binding RHS directly
    // and inserting in cell 1's body — the populated Map lives in
    // the snapshot global until the runner dies.
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured(
        "let mut m: Map[i64, i64] = Map.new(); m.insert(1, 100); println(\"called\");",
    );
    assert!(r.errors.is_empty(), "cell 1: {:?}", r.errors);
    assert_eq!(
        r.stdout.trim(),
        "called",
        "cell 1 should print the side effect once"
    );
    let r =
        s.evaluate_cell_captured("match m.get(1) { Some(v) => println(v), None => println(-1), }");
    assert!(r.errors.is_empty(), "cell 2: {:?}", r.errors);
    assert_eq!(
        r.stdout.trim(),
        "100",
        "cell 2 should see cell 1's inserted entry via the snapshot global",
    );
}

#[test]
fn repl_jit_map_cross_cell_shadow_drops_runner() {
    // Slice c-repl.B.5.3b — Map entries land in `jit_snapshotted_lets`
    // the same way primitive/String/Vec entries do, so the cross-cell
    // shadow detection in `prune_shadowed_lets` (B.5.1 follow-up)
    // picks them up uniformly. Cell 1 binds a Map[i64, i64]; cell 2
    // rebinds the same name to a different Map without `:reset`. The
    // shadow detection drops the runner, the fresh runner re-captures
    // cell 2's new value, and the use cell observes the new entry.
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured("let mut m: Map[i64, i64] = Map.new(); m.insert(1, 7);");
    assert!(r.errors.is_empty(), "cell 1: {:?}", r.errors);
    let r = s.evaluate_cell_captured(
        "let mut m: Map[i64, i64] = Map.new(); m.insert(1, 42); \
         match m.get(1) { Some(v) => println(v), None => println(-1), }",
    );
    assert!(r.errors.is_empty(), "cell 2: {:?}", r.errors);
    assert_eq!(
        r.stdout.trim(),
        "42",
        "cross-cell Map shadow must re-capture, not replay cell 1's stale handle; stdout: {:?}",
        r.stdout,
    );
}

#[test]
fn repl_jit_map_mut_let_carries_in_cell_mutation_across_cells() {
    // `let mut` container bindings used to be EXCLUDED from the snapshot
    // path (the slice-B.5.2/B.5.3 "mut filter"), on the reasoning that
    // capture's cap-zero ownership transfer would break a same-cell
    // mutation that ran after it. That was true only because capture fired
    // at the `let`; it now fires at END OF CELL, after every statement, so
    // the exclusion is gone (B-2026-07-29-20). Its real effect was the bug:
    // with no capture, cell N+1 re-evaluated the RHS and the mutation
    // vanished. Both halves are asserted here — the same-cell mutation
    // still works, AND it survives the cell boundary.
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured(
        "let mut m: Map[i64, i64] = Map.new(); m.insert(1, 100); \
         match m.get(1) { Some(v) => println(v), None => println(-1), }",
    );
    assert!(
        r.errors.is_empty(),
        "mut Map cell should run cleanly: {:?}",
        r.errors,
    );
    assert_eq!(r.stdout.trim(), "100");
    let r2 =
        s.evaluate_cell_captured("match m.get(1) { Some(v) => println(v), None => println(-1), }");
    assert!(
        r2.errors.is_empty(),
        "cell 2 should run cleanly: {:?}",
        r2.errors,
    );
    assert_eq!(
        r2.stdout.trim(),
        "100",
        "the same-cell insert must survive the cell boundary (was -1 \
         when the RHS got re-evaluated)"
    );
}

#[test]
fn repl_jit_set_let_rhs_is_not_re_evaluated() {
    // Slice c-repl.B.5.3c friction probe — Set[primitive] cross-cell
    // let snapshot. Mirrors B.5.3b's Map probe shape: cell 1 binds a
    // Set via `Set.new()` and inserts an entry in the same cell; cell
    // 2 reads via `s.contains(1)`. Persistent-let replay re-emits the
    // `let s = Set.new();` into cell 2's synth source. Pre-B.5.3c the
    // JIT path lacks Set snapshot support, so the replayed Set.new()
    // produces a fresh empty handle → `contains(1)` returns false →
    // prints 0. Post-B.5.3c the snapshot mechanism replays cell 1's
    // populated handle → `contains(1)` returns true → prints 1.
    //
    // Set.new() shares the Map[K, V] runtime (`karac_map_new` with
    // val_size = 0, single opaque handle), so the storage layout is
    // identical to B.5.3b. We sidestep the fn-return path for the
    // same reason the Map probe did (Set-returned-from-fn surfaces
    // the same `FreeMapHandle` tail-return path, which we already
    // fixed for Map; the inline shape is the cleaner probe).
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured(
        "let mut s: Set[i64] = Set.new(); s.insert(1); println(\"called\");",
    );
    assert!(r.errors.is_empty(), "cell 1: {:?}", r.errors);
    assert_eq!(
        r.stdout.trim(),
        "called",
        "cell 1 should print the side effect once"
    );
    let r = s.evaluate_cell_captured("if s.contains(1) { println(1); } else { println(0); }");
    assert!(r.errors.is_empty(), "cell 2: {:?}", r.errors);
    assert_eq!(
        r.stdout.trim(),
        "1",
        "cell 2 should see cell 1's inserted element via the snapshot global",
    );
}

#[test]
fn repl_jit_set_cross_cell_shadow_drops_runner() {
    // Slice c-repl.B.5.3c — Set entries land in `jit_snapshotted_lets`
    // the same way primitive/String/Vec/Map entries do, so the cross-
    // cell shadow detection in `prune_shadowed_lets` (B.5.1 follow-up)
    // picks them up uniformly. Cell 1 binds a Set[i64]; cell 2 rebinds
    // the same name to a different Set without `:reset`. The shadow
    // detection drops the runner, the fresh runner re-captures cell
    // 2's new value, and the use cell observes the new element.
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured("let mut s: Set[i64] = Set.new(); s.insert(1);");
    assert!(r.errors.is_empty(), "cell 1: {:?}", r.errors);
    let r = s.evaluate_cell_captured(
        "let mut s: Set[i64] = Set.new(); s.insert(42); \
         if s.contains(42) { println(42); } else { println(-1); }",
    );
    assert!(r.errors.is_empty(), "cell 2: {:?}", r.errors);
    assert_eq!(
        r.stdout.trim(),
        "42",
        "cross-cell Set shadow must re-capture, not replay cell 1's stale handle; stdout: {:?}",
        r.stdout,
    );
}

#[test]
fn repl_jit_set_mut_let_carries_in_cell_mutation_across_cells() {
    // `let mut` container bindings used to be EXCLUDED from the snapshot
    // path (the slice-B.5.2/B.5.3 "mut filter"), on the reasoning that
    // capture's cap-zero ownership transfer would break a same-cell
    // mutation that ran after it. That was true only because capture fired
    // at the `let`; it now fires at END OF CELL, after every statement, so
    // the exclusion is gone (B-2026-07-29-20). Its real effect was the bug:
    // with no capture, cell N+1 re-evaluated the RHS and the mutation
    // vanished. Both halves are asserted here — the same-cell mutation
    // still works, AND it survives the cell boundary.
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured(
        "let mut s: Set[i64] = Set.new(); s.insert(1); s.insert(2); \
         if s.contains(2) { println(2); } else { println(-1); }",
    );
    assert!(
        r.errors.is_empty(),
        "mut Set cell should run cleanly: {:?}",
        r.errors,
    );
    assert_eq!(r.stdout.trim(), "2");
    let r2 = s.evaluate_cell_captured("if s.contains(2) { println(2); } else { println(-1); }");
    assert!(
        r2.errors.is_empty(),
        "cell 2 should run cleanly: {:?}",
        r2.errors,
    );
    assert_eq!(
        r2.stdout.trim(),
        "2",
        "the same-cell inserts must survive the cell boundary (was -1 \
         when the RHS got re-evaluated)"
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

#[test]
fn repl_jit_cross_type_rebind_uses_new_value() {
    // Cross-TYPE cross-cell rebind — the JIT analog of the interpreter
    // inspector test `let_value_snapshot_rebinding_drops_stale_entry`.
    // The same-type shadow tests above (`..cross_cell_shadow_clears_
    // snapshot` i64→i64, `..string_cross_cell_shadow_drops_runner`
    // String→String) prove the snapshot global is dropped on rebind,
    // but only within one type. This pins the *type-confusion* guard:
    // cell 1 binds `x: i64`, cell 2 rebinds `x` to a `String`. If the
    // shadow-drop failed to evict `@__karac_repl_snapshot_x`, cell 2's
    // classifier would route the String rebind through REPLAY and load
    // the stale i64 bit-pattern where a `(ptr, len, cap)` String is
    // expected — a runtime type-confusion. Correct behavior: the rebind
    // re-captures and prints the new String value.
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured("let x = 5;");
    assert!(r.errors.is_empty(), "cell 1 (i64 bind): {:?}", r.errors);
    let r = s.evaluate_cell_captured("let x: String = \"hello\"; println(x);");
    assert!(
        r.errors.is_empty(),
        "cell 2 (String rebind): {:?}",
        r.errors
    );
    assert_eq!(
        r.stdout.trim(),
        "hello",
        "cross-type rebind must drop the stale i64 snapshot and use the new \
         String value, not replay; stdout: {:?}",
        r.stdout,
    );
}

// ── Slice 5: JIT-default flip + `--interp` escape hatch ─────────────────────

/// `--interp` (surfaced as `ReplOptions.interp`) forces the tree-walk
/// interpreter over the now-default JIT. `Session::with_options` reads the
/// env-derived default in `new()` (JIT-on unless `KARAC_REPL_JIT=0`) and then
/// the flag hard-overrides it off. This is the regression guard for the
/// escape hatch — the flag must win regardless of the ambient default.
#[test]
fn repl_interp_flag_forces_interpreter_over_default_jit() {
    use karac::repl::ReplOptions;
    let s = Session::with_options(ReplOptions {
        auto_clone: false,
        interp: true,
    });
    assert!(
        !s.jit_enabled(),
        "--interp must force the interpreter (jit_enabled == false) even though \
         the Slice-5 default is JIT-on"
    );
}

/// Without `--interp`, `with_options` leaves the JIT default in place: the
/// Slice-5 flip means a fresh session is JIT-enabled unless `KARAC_REPL_JIT=0`
/// is set. This suite's tests do not set that env var, so the default holds.
#[test]
fn repl_default_is_jit_after_slice5_flip() {
    use karac::repl::ReplOptions;
    // Guard the assertion on the escape-hatch env being unset, so a caller
    // that exports KARAC_REPL_JIT=0 in the environment doesn't spuriously fail
    // this test (the flag/env opt-outs are exercised by the test above).
    if std::env::var("KARAC_REPL_JIT").as_deref() == Ok("0") {
        return;
    }
    let s = Session::with_options(ReplOptions {
        auto_clone: false,
        interp: false,
    });
    assert!(
        s.jit_enabled(),
        "post-Slice-5, the default repl backend is the JIT (jit_enabled == true) \
         unless --interp / KARAC_REPL_JIT=0 opt out"
    );
}

/// Regression: the INTERACTIVE `karac repl` (the real binary, driven over
/// piped stdin — the `capture=false` path) must actually PRINT a cell's
/// `println` output to the terminal under the JIT-default backend.
///
/// The rest of this file drives `Session::evaluate_cell_captured`
/// (`capture=true`), which hands the runner-captured bytes back to the
/// caller — so it never exercised the interactive path, where the runner
/// pipes the cell's stdout and the REPL must forward it. That forward was
/// missing (the non-capture arms returned `Vec::new()` and dropped the
/// bytes), so after the Slice-5 JIT-default flip an interactive `println`
/// cell was SILENT. This test spawns the actual `karac repl` process the
/// way a user does and asserts the output reaches stdout. See
/// B-2026-07-09-3.
#[test]
fn repl_jit_interactive_subprocess_prints_cell_output() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("repl")
        .env("KARAC_JIT_RUNNER", env!("CARGO_BIN_EXE_karac_jit_runner"))
        // Ensure the JIT default holds even if the ambient env opted out.
        .env_remove("KARAC_REPL_JIT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn karac repl");

    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(b"let a = 40;\nprintln(a + 2);\nprintln(\"hi\");\n:quit\n")
        .expect("write repl stdin");

    let out = child.wait_with_output().expect("wait karac repl");
    let stdout = String::from_utf8_lossy(&out.stdout);

    // The banner confirms the JIT backend is actually the one under test.
    assert!(
        stdout.contains("(JIT"),
        "expected the JIT banner (JIT default); full stdout:\n{stdout}"
    );
    // The two cells' output must both reach the terminal.
    assert!(
        stdout.contains("42") && stdout.contains("hi"),
        "interactive JIT repl must print cell output '42' and 'hi'; got stdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Regression (B-2026-07-09-4): a cell that FAULTS under the JIT (an OOB,
/// contract violation, failed assert — all lower to `emit_panic` → libc
/// `exit(1)`) must still show its output to an interactive user: the prints
/// it made before the fault AND the `panic …` message itself. Before the
/// runner's atexit salvage, the runner died mid-cell before framing its
/// captured stdout, so the parent got an empty runner-died signal and the
/// fault was silent. This drives the real `karac repl` binary over piped
/// stdin and asserts the panic text reaches the terminal and that a later
/// cell still runs (the client re-spawned the runner).
#[test]
fn repl_jit_interactive_subprocess_surfaces_panic_and_recovers() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("repl")
        .env("KARAC_JIT_RUNNER", env!("CARGO_BIN_EXE_karac_jit_runner"))
        .env_remove("KARAC_REPL_JIT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn karac repl");

    // Cell 1 prints, then faults on an OOB; cell 2 must still run after the
    // runner re-spawns.
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(b"let v: Vec[i64] = Vec.new();\nprintln(111); println(v[5]); println(999);\nprintln(444);\n:quit\n")
        .expect("write repl stdin");

    let out = child.wait_with_output().expect("wait karac repl");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("111"),
        "pre-fault print should reach the terminal; got:\n{combined}"
    );
    assert!(
        combined.contains("panic") && combined.contains("out of bounds"),
        "the OOB panic text must be salvaged to the terminal; got:\n{combined}"
    );
    assert!(
        !combined.contains("999"),
        "execution must halt at the fault (999 is after v[5]); got:\n{combined}"
    );
    assert!(
        combined.contains("444"),
        "a later cell must run after the runner re-spawns; got:\n{combined}"
    );
}

/// B-2026-08-30-7 — the INLINE-ENVELOPE half of the snapshot tier: an
/// `Option` / `Result` whose payload owns a `{ptr,len,cap}` buffer must carry
/// a mutation across the cell boundary.
///
/// The row's earlier updates closed the by-value shapes (`Option[i64]`) and
/// the container shapes (`Vec[String]`, `Map[String, i64]`). This is the
/// nearest adjacent one the row names and explicitly leaves open: storage was
/// never the obstacle — a `{ptr,len,cap}` payload fits the seeded
/// `{tag, w0, w1, w2}` envelope exactly — only suppression was, since
/// `zero_vec_alloca_cap` GEPs a vec struct and an enum payload word is not
/// one. `retract_inline_envelope_payload_cleanup` is the answer, and it is the
/// Map/Set retraction shape rather than the String/Vec sentinel shape.
///
/// The `mutate` cell is deliberately the one AFTER the declaring cell: that is
/// the shape the row's own table measures, and it exercises the write-back on
/// a REPLAYED binding rather than only on a first one.
#[test]
fn repl_jit_inline_envelope_tier_carries_a_heap_payload_across_cells() {
    for (decl, mutate, read, want) in [
        // The row's headline shape for this half.
        (
            "let mut o: Option[String] = Option.None;",
            "o = Option.Some(f\"hi\");",
            "println(f\"{o.is_some()}\");",
            "true",
        ),
        // …and reading the payload back out, not just the tag. A tag-only
        // assertion would pass on a snapshot that captured a dangling pointer.
        (
            "let mut o: Option[String] = Option.Some(f\"first\");",
            "o = Option.Some(f\"second\");",
            "match o { Option.Some(s) => println(s), Option.None => println(f\"none\") }",
            "second",
        ),
        // A `Vec` payload: the same triple, reached through the overlay's own
        // one-level recursion rather than the `String` arm.
        (
            "let mut o: Option[Vec[i64]] = Option.None;",
            "o = Option.Some(Vec.new());",
            "println(f\"{o.is_some()}\");",
            "true",
        ),
        // Both `Result` halves heap-carrying.
        (
            "let mut r: Result[String, String] = Result.Err(f\"e\");",
            "r = Result.Ok(f\"good\");",
            "println(f\"{r.is_ok()}\");",
            "true",
        ),
        // Only the `Err` half heap-carrying — the mixed shape, which is where
        // a per-half eligibility bug would show up.
        (
            "let mut r: Result[i64, String] = Result.Ok(1);",
            "r = Result.Err(f\"bad\");",
            "println(f\"{r.is_err()}\");",
            "true",
        ),
    ] {
        let mut s = Session::new();
        enable_jit(&mut s);
        let r = s.evaluate_cell_captured(decl);
        assert!(r.errors.is_empty(), "{decl}: {:?}", r.errors);
        assert!(
            !r.notes.iter().any(|n| n.contains("repl-jit-no-snapshot")),
            "{decl} is an inline envelope and must reach the snapshot tier; \
             notes: {:?}",
            r.notes,
        );
        let r = s.evaluate_cell_captured(mutate);
        assert!(r.errors.is_empty(), "{mutate}: {:?}", r.errors);
        let r = s.evaluate_cell_captured(read);
        assert!(r.errors.is_empty(), "{read}: {:?}", r.errors);
        assert_eq!(
            r.stdout.trim(),
            want,
            "a mutation made in the cell AFTER the declaring one must survive \
             the boundary for `{decl}`; stdout: {:?}",
            r.stdout,
        );
    }
}

/// B-2026-08-30-7 — a CONSUMING arm over a snapshotted inline envelope must
/// borrow its payload, not free it.
///
/// This is the regression the widening itself introduced and had to fix, and
/// it is worth its own test because the failure it guards is strictly WORSE
/// than the divergence the widening removes. `inline_option_payload_vars` is
/// an input to the arm's borrow-vs-consume classification; a replayed binding
/// missing from it is classified CONSUMING, so `match o { Option.Some(s) =>
/// println(s) }` frees the payload buffer at arm exit while the snapshot
/// global still points at it. Measured on the transfer-only version: cell 2's
/// match printed `alpha`, cell 3's died with "JIT runner subprocess died
/// mid-cell". The same two matches as one AOT program are clean under
/// valgrind — there `o` is a real local the classification can see is live.
///
/// THREE reads, not two: the first read is what frees, so a two-cell version
/// passes on the broken build.
///
/// GREEN on pre-fix `main`, and that is not a weakness: there these shapes are
/// on pass-through, so every cell rebuilds the binding from its constant
/// initializer and prints the right thing by accident. The test is red only on
/// the intermediate transfer-only version — it guards a regression the
/// widening introduces, not a divergence that predates it.
#[test]
fn repl_jit_inline_envelope_consuming_arm_does_not_free_the_snapshot() {
    for (decl, read, want) in [
        (
            "let mut o: Option[String] = Option.Some(f\"alpha\");",
            "match o { Option.Some(s) => println(s), Option.None => println(f\"none\") }",
            "alpha",
        ),
        (
            "let mut o: Option[String] = Option.Some(f\"alpha\");",
            "if let Option.Some(s) = o { println(s); }",
            "alpha",
        ),
        (
            "let mut r: Result[String, String] = Result.Ok(f\"good\");",
            "match r { Result.Ok(s) => println(s), Result.Err(e) => println(e) }",
            "good",
        ),
    ] {
        let mut s = Session::new();
        enable_jit(&mut s);
        let r = s.evaluate_cell_captured(decl);
        assert!(r.errors.is_empty(), "{decl}: {:?}", r.errors);
        for round in 1..=3 {
            let r = s.evaluate_cell_captured(read);
            assert!(r.errors.is_empty(), "{read} round {round}: {:?}", r.errors);
            assert_eq!(
                r.stdout.trim(),
                want,
                "round {round} of `{read}` over `{decl}`: a consuming arm must \
                 not free a payload the snapshot global still owns; stdout: {:?}",
                r.stdout,
            );
        }
    }
}

/// B-2026-08-30-7, the SIDE-EFFECT half for the inline-envelope tier.
///
/// Same argument as `repl_jit_by_value_initializer_runs_once_across_cells`:
/// mutation loss and RHS re-execution are two divergences with one mechanism,
/// and a fix that replayed the value while still emitting the RHS would pass
/// the tier test above and fail this one. Measured before the fix:
/// `SIDE EFFECT` three times across four cells; after: once.
#[test]
fn repl_jit_inline_envelope_initializer_runs_once_across_cells() {
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured(
        "fn mk() -> Option[String] { println(f\"SIDE EFFECT\"); Option.Some(f\"x\") }",
    );
    assert!(r.errors.is_empty(), "items: {:?}", r.errors);
    let r = s.evaluate_cell_captured("let mut o: Option[String] = mk();");
    assert!(r.errors.is_empty(), "declare: {:?}", r.errors);
    assert_eq!(
        r.stdout.matches("SIDE EFFECT").count(),
        1,
        "the declaring cell runs the initializer exactly once; stdout: {:?}",
        r.stdout,
    );
    for cell in ["println(f\"two\");", "println(f\"three\");"] {
        let r = s.evaluate_cell_captured(cell);
        assert!(r.errors.is_empty(), "{cell}: {:?}", r.errors);
        assert_eq!(
            r.stdout.matches("SIDE EFFECT").count(),
            0,
            "a later cell must not re-run the initializer; stdout: {:?}",
            r.stdout,
        );
    }
}

// B-2026-08-30-7: `repl_jit_inline_envelope_tier_excludes_aggregate_and_shared_
// payloads` lived here. It asserted that an `Option[<user struct>]` and an
// `Option[<shared>]` stay on pass-through, which was true when the
// `InlineEnvelope` kind was the deepest tier and is FALSE now — the general
// `SlotTransfer` kind admits both, and they are covered by
// `repl_jit_slot_transfer_tier_carries_the_remaining_shapes` and
// `repl_jit_slot_transfer_admits_shared_as_a_binding_and_as_a_component`.
// Removed rather than retargeted: what it really pinned was an internal
// choice of KIND, which has no observable outside the tier it was measuring.

/// B-2026-08-30-7 — the GENERAL `SlotTransfer` tier: everything the row's
/// remaining list named, closed by one kind rather than six.
///
/// The framing this tier was deferred under — "each of those types would need
/// its own cross-cell ownership story" — asked the wrong question. The question
/// is not what a type's cleanup DOES, it is whether the cleanup is reachable
/// from the binding's SLOT; and every action that frees a local's value is
/// registered by a `track_*` call taking that local's slot. So one
/// `retract_all_cleanup_for_slot` at capture covers a nested container, a
/// `SortedMap`, a closure and a stdlib value enum alike.
///
/// Each row's mutation is in the cell AFTER the declaring one — the row's own
/// table shape, and the one that exercises the write-back on a REPLAYED
/// binding rather than only on a first one. A row with an empty `mutate` has
/// nothing to mutate (a closure) and reads twice instead.
#[test]
fn repl_jit_slot_transfer_tier_carries_the_remaining_shapes() {
    for (items, decl, mutate, read, want) in [
        // Nested containers — the `Vec[Vec[i64]]` the row named. The
        // deferral said the one-level `cap = 0` "does not reach a nested
        // triple's suppression"; it does not need to, because retraction
        // removes the whole walk that would have reached it.
        (
            "",
            "let mut v: Vec[Vec[i64]] = Vec.new();",
            "v.push(Vec.new());",
            "println(f\"{v.len()}\");",
            "1",
        ),
        (
            "",
            "let mut m: Map[i64, Vec[i64]] = Map.new();",
            "m.insert(1, Vec.new());",
            "println(f\"{m.len()}\");",
            "1",
        ),
        // A container over a user struct that owns heap.
        (
            "struct H { n: String }\nfn mkh() -> H { H { n: f\"x\" } }",
            "let mut v: Vec[H] = Vec.new();",
            "v.push(mkh());",
            "println(f\"{v.len()}\");",
            "1",
        ),
        // `SortedSet` / `SortedMap` — B-tree-backed, so they could not
        // piggyback on the Map/Set handle story and were listed separately.
        (
            "",
            "let mut s: SortedSet[i64] = SortedSet.new();",
            "s.insert(3);",
            "println(f\"{s.len()}\");",
            "1",
        ),
        (
            "",
            "let mut m: SortedMap[String, i64] = SortedMap.new();",
            "m.insert(f\"k\", 4);",
            "println(f\"{m.len()}\");",
            "1",
        ),
        // A stdlib VALUE enum. The by-value tier measured `Ordering` as
        // genuinely by-value and still kept it out, because its
        // session-declared rule could not tell a stdlib value type from a
        // stdlib opaque handle. The whitelist is that answer.
        (
            "",
            "let mut o: Ordering = Ordering.Less;",
            "o = Ordering.Greater;",
            "println(f\"{o == Ordering.Greater}\");",
            "true",
        ),
        // An AGGREGATE `Option` payload — declined by `InlineEnvelope`,
        // whose overlay does not own a struct payload's cleanup.
        (
            "struct H { n: String }\nfn mkh() -> H { H { n: f\"x\" } }",
            "let mut o: Option[H] = Option.None;",
            "o = Option.Some(mkh());",
            "println(f\"{o.is_some()}\");",
            "true",
        ),
        // A nested envelope.
        (
            "",
            "let mut o: Option[Option[String]] = Option.None;",
            "o = Option.Some(Option.Some(f\"deep\"));",
            "println(f\"{o.is_some()}\");",
            "true",
        ),
        // A tuple with a heap element — the by-value tuple arm requires
        // every element by-value, so this fell through to pass-through.
        (
            "",
            "let mut t: (i64, String) = (1, f\"a\");",
            "t = (2, f\"b\");",
            "println(f\"{t.0}\");",
            "2",
        ),
        // A closure: a fat `{fn_ptr, env_ptr}` whose environment is freed by
        // one `FreeClosureEnv` on the slot.
        (
            "",
            "let cf = |x: i64| { x + 1 };",
            "",
            "println(f\"{cf(7)}\");",
            "8",
        ),
    ] {
        let mut s = Session::new();
        enable_jit(&mut s);
        if !items.is_empty() {
            let r = s.evaluate_cell_captured(items);
            assert!(r.errors.is_empty(), "{items}: {:?}", r.errors);
        }
        let r = s.evaluate_cell_captured(decl);
        assert!(r.errors.is_empty(), "{decl}: {:?}", r.errors);
        assert!(
            !r.notes.iter().any(|n| n.contains("repl-jit-no-snapshot")),
            "{decl} must reach the snapshot tier; notes: {:?}",
            r.notes,
        );
        if !mutate.is_empty() {
            let r = s.evaluate_cell_captured(mutate);
            assert!(r.errors.is_empty(), "{mutate}: {:?}", r.errors);
        }
        let r = s.evaluate_cell_captured(read);
        assert!(r.errors.is_empty(), "{read}: {:?}", r.errors);
        assert_eq!(
            r.stdout.trim(),
            want,
            "a mutation made in the cell AFTER the declaring one must survive \
             the boundary for `{decl}`; stdout: {:?}",
            r.stdout,
        );
    }
}

/// B-2026-08-30-7 — a snapshotted `SlotTransfer` binding must survive being
/// READ repeatedly across cells, not merely written once.
///
/// This is the guard the previous half of this row taught. There, retraction
/// alone let a consuming `match` arm free a payload the snapshot global still
/// owned: cell 2 printed the right thing and cell 3 killed the runner. A tier
/// test that reads once cannot see that, because the first read is what frees.
/// Three reads is the minimum that can.
///
/// The shapes here are the ones where a read reaches THROUGH the transferred
/// storage — an element of a nested container, a field of a struct element, an
/// aggregate payload bound out by an arm — rather than just its length.
#[test]
fn repl_jit_slot_transfer_repeated_reads_do_not_free_the_snapshot() {
    for (items, setup, read, want) in [
        (
            "",
            &[
                "let mut v: Vec[Vec[i64]] = Vec.new();",
                "let mut i: Vec[i64] = Vec.new();",
                "i.push(7);",
                "v.push(i);",
            ][..],
            "println(f\"{v[0][0]}\");",
            "7",
        ),
        (
            "struct H { n: String }\nfn mkh() -> H { H { n: f\"x\" } }",
            &["let mut v: Vec[H] = Vec.new();", "v.push(mkh());"][..],
            "println(v[0].n);",
            "x",
        ),
        (
            "struct H { n: String }\nfn mkh() -> H { H { n: f\"deep\" } }",
            &["let mut o: Option[H] = Option.Some(mkh());"][..],
            "match o { Option.Some(h) => println(h.n), Option.None => println(f\"none\") }",
            "deep",
        ),
        (
            "",
            &["let mut t: (i64, String) = (1, f\"a\");"][..],
            "println(t.1);",
            "a",
        ),
    ] {
        let mut s = Session::new();
        enable_jit(&mut s);
        if !items.is_empty() {
            let r = s.evaluate_cell_captured(items);
            assert!(r.errors.is_empty(), "{items}: {:?}", r.errors);
        }
        for cell in setup {
            let r = s.evaluate_cell_captured(cell);
            assert!(r.errors.is_empty(), "{cell}: {:?}", r.errors);
        }
        for round in 1..=3 {
            let r = s.evaluate_cell_captured(read);
            assert!(r.errors.is_empty(), "{read} round {round}: {:?}", r.errors);
            assert_eq!(
                r.stdout.trim(),
                want,
                "round {round} of `{read}`: a transferred value must stay \
                 readable — the global owns it and nothing in the cell may \
                 free it; stdout: {:?}",
                r.stdout,
            );
        }
    }
}

/// B-2026-08-30-7 — the `SlotTransfer` tier's exclusions, which are what keep
/// the generality honest.
///
/// Three kinds of decline, each for its own reason:
///   - a user `impl Drop` ANYWHERE inside the type, including behind a field
///     of an element. Retraction means the destructor never runs, where
///     `--interp` keeps a real `Value` and drops it — trading this row's
///     divergence for a new one. The nested rows are the load-bearing ones: a
///     shallow "does this type have a Drop impl" check passes them.
///   - a stdlib name outside `SLOT_TRANSFER_STDLIB_TYPES`. `DataFrame` and
///     `Interner` are opaque handles whose declared shape is a lie about their
///     storage; others hold an OS resource whose release is observable. The
///     guard is a whitelist so an unlisted name fails CLOSED, back to
///     pass-through.
///   - a binding whose own type is `shared`. The transfer argument holds for
///     it (retracting the dec HOLDS the reference), but REPLAY does not:
///     `register_var_from_type_expr` re-registers the name as an inline
///     struct, so `s.n` GEPs the slot and reads the pointer's own bits.
///     Measured garbage where `--interp` printed `3`. Tracked as B-2026-09-01-36.
#[test]
fn repl_jit_slot_transfer_tier_declines_drop_bearing_and_unlisted_types() {
    let drop_impl =
        "struct D { n: i64 }\nimpl Drop for D { fn drop(mut ref self) { println(f\"dD\"); } }";
    for (items, decl) in [
        (drop_impl.to_string(), "let mut v: Vec[D] = Vec.new();"),
        // Drop one level down, behind a wrapper's field.
        (
            format!("{drop_impl}\nstruct W {{ d: D }}"),
            "let mut v: Vec[W] = Vec.new();",
        ),
        (
            format!("{drop_impl}\nstruct W {{ d: D }}"),
            "let mut o: Option[W] = Option.None;",
        ),
        (drop_impl.to_string(), "let mut m: Map[i64, D] = Map.new();"),
        // Opaque stdlib handles — not on the whitelist.
        (String::new(), "let mut d = DataFrame.new();"),
        (String::new(), "let mut i = Interner.new();"),
    ] {
        let mut s = Session::new();
        enable_jit(&mut s);
        if !items.is_empty() {
            let r = s.evaluate_cell_captured(&items);
            assert!(r.errors.is_empty(), "{items}: {:?}", r.errors);
        }
        let r = s.evaluate_cell_captured(decl);
        assert!(r.errors.is_empty(), "{decl}: {:?}", r.errors);
        assert!(
            r.notes.iter().any(|n| n.contains("repl-jit-no-snapshot")),
            "{decl} must stay on pass-through; notes: {:?}",
            r.notes,
        );
    }
}

/// B-2026-09-01-36 — a `shared` binding rides the tier at the TOP LEVEL too,
/// not only as a component.
///
/// This test previously pinned the OPPOSITE: a direct `shared` binding was
/// declined, on the reading that admitting it "printed a pointer's bits". The
/// garbage was real, but the cause recorded for it was not — see
/// `snapshot_kind_for_type` for what was actually measured. Once the `RcDec`
/// retraction stopped missing (it is keyed by the RC object, not the slot, so a
/// slot-keyed retraction never matched it and the object was freed under the
/// snapshot global), the shape reads correctly and is admitted.
///
/// The COMPONENT half is unchanged and still load-bearing in the other
/// direction: narrowing the tier to exclude `shared` as a component would
/// silently drop `struct W { s: S }` and `Vec[S]` back to pass-through.
#[test]
fn repl_jit_slot_transfer_admits_shared_as_a_binding_and_as_a_component() {
    let items = "shared struct S { n: i64 }\nstruct W { s: S }\nfn mks() -> S { S { n: 6 } }";
    for decl in [
        // Component — was already admitted, must stay so.
        "let mut v: Vec[S] = Vec.new();",
        "let mut vw: Vec[W] = Vec.new();",
        // Binding — the shape this row admits.
        "let sv: S = S { n: 3 };",
    ] {
        let mut s = Session::new();
        enable_jit(&mut s);
        let r = s.evaluate_cell_captured(items);
        assert!(r.errors.is_empty(), "items: {:?}", r.errors);
        let r = s.evaluate_cell_captured(decl);
        assert!(r.errors.is_empty(), "{decl}: {:?}", r.errors);
        assert!(
            !r.notes.iter().any(|n| n.contains("repl-jit-no-snapshot")),
            "`{decl}` must reach the snapshot tier; notes: {:?}",
            r.notes,
        );
    }

    // Reading the binding twice is the shape that used to print two DIFFERENT
    // garbage values — the tell that it was reading freed memory rather than a
    // stale copy.
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured("shared struct S { n: i64 }");
    assert!(r.errors.is_empty(), "items: {:?}", r.errors);
    let r = s.evaluate_cell_captured("let sv: S = S { n: 3 };");
    assert!(r.errors.is_empty(), "decl: {:?}", r.errors);
    for round in 1..=2 {
        let r = s.evaluate_cell_captured("println(f\"{sv.n}\");");
        assert!(r.errors.is_empty(), "read round {round}: {:?}", r.errors);
        assert_eq!(
            r.stdout.trim(),
            "3",
            "round {round}; stdout: {:?}",
            r.stdout
        );
    }
}

/// B-2026-09-01-36 — the fixture that actually SEPARATES the tier from
/// pass-through, and the reason this row is a correctness fix rather than a
/// tidy-up.
///
/// A plain read cannot tell them apart: pass-through re-evaluates the
/// declaring cell, so it reproduces the initializer and prints `3` either way.
/// A MUTATION in a later cell can. Measured on the pre-fix binary, with the
/// shape declined: the JIT printed `3` — the initializer, re-evaluated, with
/// the mutation lost — where `--interp` printed `7`. So the decline was not
/// merely conservative; it was preserving a run-vs-interp wrong answer.
///
/// (The original row predicted `1` for this spelling on its partially-admitted
/// tree. Measured here it is `3` declined and `7` admitted, which is why the
/// number is pinned from a run rather than quoted.)
#[test]
fn repl_jit_a_shared_binding_keeps_a_later_cells_mutation() {
    let mut s = Session::new();
    enable_jit(&mut s);
    let r = s.evaluate_cell_captured("shared struct S { mut n: i64 }");
    assert!(r.errors.is_empty(), "items: {:?}", r.errors);
    let r = s.evaluate_cell_captured("let sv: S = S { n: 3 };");
    assert!(r.errors.is_empty(), "decl: {:?}", r.errors);
    let r = s.evaluate_cell_captured("sv.n = 7;");
    assert!(r.errors.is_empty(), "mutate: {:?}", r.errors);
    let r = s.evaluate_cell_captured("println(f\"{sv.n}\");");
    assert!(r.errors.is_empty(), "read: {:?}", r.errors);
    assert_eq!(
        r.stdout.trim(),
        "7",
        "a mutation in a later cell must survive; `3` means the binding fell \
         back to pass-through and re-ran its initializer. stdout: {:?}",
        r.stdout,
    );
}
