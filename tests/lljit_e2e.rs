//! Phase-7 L560 W3.1: JIT-based E2E test harness.
//!
//! Originally W3.1 used in-process `dup`/`dup2` to redirect fd 1 around
//! a JIT'd `main` call so stdout could be captured. That model raced
//! cargo's libtest runner writes against the per-test redirect under
//! the default parallel `--test-threads`, surfacing as flaky
//! cross-test stdout leakage. Ported to spawn `karac_jit_runner` in
//! one-shot mode (same helper `tests/codegen.rs::jit_dispatch` uses):
//! each test gets its own subprocess with its own fd table, so the
//! libtest-writer-vs-redirect race is structurally impossible.
//!
//! The "in-process JIT" promise still lives in production (`karac run
//! foo.kara` is true in-process) and is independently exercised by
//! `tests/lljit_prototype.rs`'s engine-level lifecycle tests. The
//! E2E suite below uses subprocess JIT as a test-runner artifact —
//! parallel to how the AOT codegen suite already spawns compiled
//! binaries.

#![cfg(feature = "llvm")]

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use karac::codegen::compile_to_ir;

mod common;

static IR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// JIT-route a Kāra program through `karac_jit_runner` and capture its
/// stdout. Mirrors `tests/codegen.rs::codegen_tests::run_program`'s
/// return shape (`Option<String>`).
///
/// Returns `Some(stdout)` if the helper spawns + runs. `None` indicates
/// the helper binary couldn't be spawned at all (unexpected on the host
/// platforms we care about); matches `output_with_hang_watchdog`'s
/// soft-skip contract for missing dependencies.
fn jit_run_program(src: &str) -> Option<String> {
    jit_run_program_capturing(src).map(|(out, _exit)| out)
}

/// Captured stdout + the JIT'd `main`'s C-ABI exit code. Mirrors what
/// the AOT path's `Output` exposes via `Command::output()`.
fn jit_run_program_capturing(src: &str) -> Option<(String, i32)> {
    let mut parsed = karac::parse(src);
    if !parsed.errors.is_empty() {
        let mut msg = String::from("test source failed to parse:\n");
        for e in &parsed.errors {
            msg.push_str(&format!("  {:?}\n", e));
        }
        panic!("{}", msg);
    }
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);

    let ir = compile_to_ir(&parsed.program, None, None).expect("compile_to_ir");

    let id = IR_COUNTER.fetch_add(1, Ordering::Relaxed);
    let ir_path = format!("/tmp/karac_jit_e2e_{}_{}.ll", std::process::id(), id);
    {
        let mut f = std::fs::File::create(&ir_path).expect("create IR tempfile");
        f.write_all(ir.as_bytes()).expect("write IR");
    }

    // `CARGO_BIN_EXE_<name>` is a cargo-set compile-time env var
    // resolving to the helper binary's path. Cargo guarantees the bin
    // target is built before the test crate, so no runtime path-hunting.
    let runner = env!("CARGO_BIN_EXE_karac_jit_runner");
    let mut cmd = std::process::Command::new(runner);
    cmd.arg(&ir_path);

    let output = common::output_with_hang_watchdog(cmd, Duration::from_secs(15));
    let _ = std::fs::remove_file(&ir_path);

    let output = output?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    // `ExitStatus::code()` is `None` only when the child was killed by
    // a signal — `output_with_hang_watchdog` panics in its watchdog
    // path before we reach here, so any `None` is a real signal kill
    // and -1 is a reasonable sentinel for tests that didn't expect one.
    let exit = output.status.code().unwrap_or(-1);
    Some((stdout, exit))
}

// ── W3.1 representative subset ───────────────────────────────────────
// 10 tests across the surface that drove L560 W2's design — printf
// (W1's gate), arithmetic, Vec, Map, control flow, `?`, fn calls.
// Not exhaustive — that's W3.2+. Each test mirrors a known-passing
// AOT test in tests/codegen.rs; assertions are identical.

#[test]
fn jit_e2e_println_i64() {
    let out = jit_run_program("fn main() { println(42); }").expect("jit");
    assert_eq!(out, "42\n");
}

#[test]
fn jit_e2e_println_bool() {
    let out = jit_run_program("fn main() { println(true); }").expect("jit");
    assert_eq!(out, "true\n");
}

#[test]
fn jit_e2e_println_negative_i32() {
    let out = jit_run_program("fn main() { let x: i32 = -123i32; println(x); }").expect("jit");
    assert_eq!(out, "-123\n");
}

#[test]
fn jit_e2e_arithmetic_println() {
    let out = jit_run_program("fn main() { println(2 + 3 * 4); }").expect("jit");
    assert_eq!(out, "14\n");
}

#[test]
fn jit_e2e_fn_call_println() {
    let out =
        jit_run_program("fn double(x: i64) -> i64 { x * 2 }\nfn main() { println(double(21)); }")
            .expect("jit");
    assert_eq!(out, "42\n");
}

#[test]
fn jit_e2e_while_loop_sum() {
    let src = "fn main() {\n  let mut i: i64 = 0;\n  let mut sum: i64 = 0;\n  while i < 10 {\n    sum = sum + i;\n    i = i + 1;\n  }\n  println(sum);\n}";
    let out = jit_run_program(src).expect("jit");
    assert_eq!(out, "45\n");
}

#[test]
fn jit_e2e_cross_type_shadow_rebind_prints_new_value() {
    // A same-name different-type shadow in one body (`let x = 5` then
    // `let x: String = ...`) lowers and runs correctly under the JIT — the
    // new String binding shadows the i64 and `println(x)` prints it. This
    // pins that the plain-codegen path is sound, isolating B-2026-07-07-6
    // (the analogous *REPL cross-cell* rebind crashes the runner on Linux)
    // to the REPL cell-codegen path (persistent-let replay + snapshot
    // machinery), NOT the general shadow lowering.
    let src = "fn main() {\n  let x = 5;\n  let x: String = \"hello\";\n  println(x);\n}";
    let out = jit_run_program(src).expect("jit");
    assert_eq!(out, "hello\n");
}

#[test]
fn jit_e2e_vec_push_len() {
    let src = "fn main() {\n  let v: Vec[i64] = Vec.new();\n  v.push(1);\n  v.push(2);\n  v.push(3);\n  println(v.len() as i64);\n}";
    let out = jit_run_program(src).expect("jit");
    assert_eq!(out, "3\n");
}

#[test]
fn jit_e2e_vec_iterate_sum() {
    let src = "fn main() {\n  let v: Vec[i64] = Vec.new();\n  v.push(10);\n  v.push(20);\n  v.push(30);\n  let mut sum: i64 = 0;\n  for x in v {\n    sum = sum + x;\n  }\n  println(sum);\n}";
    let out = jit_run_program(src).expect("jit");
    assert_eq!(out, "60\n");
}

#[test]
fn jit_e2e_map_insert_get() {
    let src = "fn main() {\n  let m: Map[i64, i64] = Map.new();\n  m.insert(1, 100);\n  m.insert(2, 200);\n  match m.get(2) {\n    Some(v) => println(v),\n    None => println(-1),\n  }\n}";
    let out = jit_run_program(src).expect("jit");
    assert_eq!(out, "200\n");
}

#[test]
fn jit_e2e_fstring_interpolation() {
    let src = "fn main() { let x: i64 = 7; println(f\"x = {x}\"); }";
    let out = jit_run_program(src).expect("jit");
    assert_eq!(out, "x = 7\n");
}

#[test]
fn jit_e2e_map_returned_from_fn_preserves_entries() {
    // Map tail-return cleanup suppression. Pre-fix, the `let m =
    // Map.new()` inside `make_map` registers a `track_map_var` whose
    // scope-exit `FreeMapHandle` fires before the caller receives
    // the handle — the returned Map's heap is freed and the caller
    // sees a dangling pointer. AOT masks this via post-codegen O2
    // elision of the dead-store/free pair; JIT runs pre-O2 IR and
    // exposes the bug. Surfaced during B.5.3b friction-probe
    // investigation 2026-05-30.
    //
    // Fix: `suppress_cleanup_for_tail_return` now also walks the
    // current scope's cleanup queue for a `FreeMapHandle` whose
    // `map_alloca` matches the tail Identifier's slot, and drops
    // it. Mirror of the Vec/String tail-suppression shape.
    let src = "fn make_map() -> Map[i64, i64] { \
        let m: Map[i64, i64] = Map.new(); m.insert(1, 100); m \
       }\n\
       fn main() { \
        let mp: Map[i64, i64] = make_map(); \
        match mp.get(1) { Some(v) => println(v), None => println(-1), } \
       }";
    let out = jit_run_program(src).expect("jit");
    assert_eq!(out, "100\n");
}

// ── W3.2 surface ─────────────────────────────────────────────────────
// par-blocks, `?` on Result, and other surface that depends on runtime
// symbols beyond the libc/Vec/Map base. Originally needed in-test
// KARAC_SPAWN_SITES stand-ins for the W3.2a finding; under the
// subprocess port the helper binary carries its own stand-ins and the
// test binary doesn't link against any JIT'd symbols.

#[test]
fn jit_e2e_question_mark_happy_path() {
    // `?` propagates an Ok through to the surrounding Result. Happy
    // path: `add_ten(true)` returns Ok(52), main prints 52. Exercises
    // codegen's `?` lowering + the runtime's karac_error_trace_clear
    // at startup (which the helper bin's force-link list covers).
    let src = r#"
fn parse_int(flag: bool) -> Result[i64, i64] {
    if flag { Ok(42_i64) } else { Err(99_i64) }
}
fn add_ten(flag: bool) -> Result[i64, i64] {
    let x = parse_int(flag)?;
    Ok(x + 10)
}
fn main() {
    match add_ten(true) {
        Ok(n) => println(n),
        Err(_) => println(0),
    }
}
"#;
    let out = jit_run_program(src).expect("jit");
    assert_eq!(out, "52\n");
}

#[test]
fn jit_e2e_question_mark_err_path() {
    // `?` propagates Err. Codegen emits karac_error_trace_push at the
    // failure block; runtime's atexit handler prints the trace to
    // stderr (now visible on the subprocess's exit, not at test-binary
    // exit). Stdout only carries the println output from main.
    let src = r#"
fn parse_int(flag: bool) -> Result[i64, i64] {
    if flag { Ok(42_i64) } else { Err(99_i64) }
}
fn add_ten(flag: bool) -> Result[i64, i64] {
    let x = parse_int(flag)?;
    Ok(x + 10)
}
fn main() {
    match add_ten(false) {
        Ok(_) => println(0),
        Err(e) => println(e),
    }
}
"#;
    let out = jit_run_program(src).expect("jit");
    assert_eq!(out, "99\n");
}

#[test]
fn jit_e2e_exit_code_zero_on_clean_run() {
    // A clean main exits 0; `jit_run_program_capturing` exposes that
    // explicitly. Sanity check the variant — under the subprocess
    // port the exit code comes from `Command::output`'s ExitStatus,
    // sourced from the helper binary's own `ExitCode::from(rc)` at
    // the end of `oneshot_main`.
    let (out, exit) = jit_run_program_capturing("fn main() { println(42); }").expect("jit");
    assert_eq!(out, "42\n");
    assert_eq!(exit, 0);
}

#[test]
fn jit_e2e_par_block_two_spawns() {
    // Two arms running in parallel inside a `par {}` block. The block
    // joins before returning, so both prints complete before main
    // exits. Print order itself is non-deterministic (worker thread
    // scheduling), so we sort the lines before comparison.
    let src = "fn main() {\n  par {\n    println(1);\n    println(2);\n  }\n}";
    let out = jit_run_program(src).expect("jit");
    let mut lines: Vec<&str> = out.lines().collect();
    lines.sort();
    assert_eq!(lines, vec!["1", "2"]);
}
