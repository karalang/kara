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
    karac::prepare_for_resolve(&mut parsed.program);
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
    // A module that never RAN is reported as such, not as an output mismatch.
    // Every harness-level failure inside `karac_jit_runner` (unresolved symbol,
    // module LLJIT refuses, missing `main`) carries this prefix; a program's own
    // `emit_panic` → `exit(1)` does not, so tests expecting a nonzero exit are
    // unaffected. Without this an unresolved extern reaches the caller as
    // `("", nonzero)` and surfaces as `assert_eq!(out, "42\n")` failing on an
    // empty string — the shape that cost real time in B-2026-07-16-16 and again
    // in B-2026-08-05-14 over in the self-host oracle.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("karac_jit_runner:"),
        "emitted IR FAILED TO RUN — link/JIT failure, not an output mismatch:\n{}",
        stderr.trim()
    );
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

#[test]
fn jit_e2e_cbrt_resolves_the_implementation_the_other_lanes_link() {
    // B-2026-08-30-4. `cbrt` is the one float-math name where the host has
    // TWO implementations and they disagree, so admitting it to the language
    // opened a `run == build` split on THIS lane alone — which is why the
    // pin lives here rather than only in the codegen suite's AOT lane.
    //
    // `compiler_builtins` ships a weak `cbrt`/`cbrtf` of its own, and that
    // definition wins over the platform libm's wherever a Rust object is in
    // the link. The interpreter (inside `karac`) and an AOT binary (which
    // links `libkarac_runtime.a`) therefore both call compiler_builtins',
    // while the JIT resolved the emitted call through `dlsym` — which does
    // not see a LOCAL archive symbol — and got the platform's instead.
    // Measured on x86-64 glibc: 224 of 400 sampled f64 inputs disagreed, and
    // `27.0.cbrt()` was `3` on two lanes and `3.0000000000000004` on this
    // one. `LLJITEngine::define_compiler_rt_builtins` now publishes the
    // address this binary's own `cbrt` resolves to, which is the same
    // implementation the other two lanes link.
    //
    // The expectation is COMPUTED rather than hardcoded, for the reason the
    // inverse-hyperbolic pair states: the invariant is "every lane calls one
    // implementation", not "the answer is these bits on this host". This test
    // binary is itself a Rust binary, so its `cbrt` is the very definition
    // the interpreter and AOT lanes use.
    //
    // Every input but the first differs between the two implementations at
    // BOTH widths (found by sweeping k/10 for k in 1..=2000 against a C
    // program linked with `-lm`: 121 of 2000 do), so all but two of the 18
    // lines fail if this lane ever resolves the other one again. `27.0` leads
    // because it is the legible case and differs at f64.
    extern "C" {
        fn cbrt(x: f64) -> f64;
        fn cbrtf(x: f32) -> f32;
    }
    let cases: &[f64] = &[27.0, 0.2, 1.6, 4.1, 4.7, 5.3, 7.3, 7.9, -10.6];
    let mut src = String::from("fn main() {\n");
    let mut want = String::new();
    for (i, v) in cases.iter().enumerate() {
        src.push_str(&format!("    let d{i}: f64 = {v:?};\n"));
        src.push_str(&format!("    println(d{i}.cbrt());\n"));
        src.push_str(&format!("    let s{i}: f32 = {v:?}f32;\n"));
        src.push_str(&format!("    println(s{i}.cbrt());\n"));
        let (wide, narrow) = unsafe { (cbrt(*v), cbrtf(*v as f32) as f64) };
        want.push_str(&format!("{wide}\n{narrow}\n"));
    }
    src.push_str("}\n");
    assert_eq!(jit_run_program(&src), Some(want));
}

#[test]
fn jit_e2e_every_shadowed_libm_symbol_is_exact_or_published() {
    // The STRUCTURAL half of B-2026-08-30-4. The fixture above pins one
    // symbol; this pins the rule that found it, so the next one does not have
    // to be found the same way — by a value that came out different on one of
    // four lanes and no failing test anywhere.
    //
    // THE RULE. Codegen lowers the float-math table to bare libm names, and
    // the AOT lane, the interpreter and the JIT resolve those names by three
    // different mechanisms — a C link against `libkarac_runtime.a`, this
    // crate's own Rust link, and ORC's `dlsym` over the runner process.
    // Wherever a Rust object is in the link, `compiler_builtins`' weak
    // definitions win over the platform libm's; `dlsym` cannot see them,
    // because an archive symbol arrives with LOCAL linkage. So a name is
    // SHADOWED when its statically-resolved address differs from what `dlsym`
    // hands back, and a shadowed name is one where the JIT and the other two
    // lanes call different code. That is safe only if:
    //
    //   (a) IEEE 754 specifies the result exactly, so any two conforming
    //       implementations agree bit for bit — square root, absolute value,
    //       sign copy, the integral roundings, remainder and fused
    //       multiply-add are all in this class; or
    //   (b) the symbol is republished into the JITDylib by
    //       `define_compiler_rt_builtins`, which is what `cbrt` needed.
    //
    // Anything else is a `run == build` divergence with no test on it.
    //
    // Measured here today: 20 of the 64 names are shadowed, 18 of them in
    // class (a) and `cbrt`/`cbrtf` in class (b) — which is why the exclusion
    // this row lifted only ever had one member. If a future toolchain ships
    // its own `sin` or `log`, this fails and names it.
    //
    // Taking each address is also what forces the linker to resolve the name
    // in THIS binary, the same way `define_compiler_rt_builtins` does for
    // `__muloti4`; a name the host does not have at all is reported by
    // `dlsym` as null and skipped rather than failed, since there is then
    // nothing for the two lanes to disagree about.
    macro_rules! probe {
        ($($s:ident($($a:ty),*) -> $r:ty),* $(,)?) => {{
            extern "C" { $(fn $s($(_: $a),*) -> $r;)* }
            let mut out: Vec<(&'static str, usize, usize)> = Vec::new();
            $({
                let statically = $s as *const () as usize;
                let name = concat!(stringify!($s), "\0");
                let via_dlsym = unsafe {
                    libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr() as *const libc::c_char)
                } as usize;
                out.push((stringify!($s), statically, via_dlsym));
            })*
            out
        }};
    }

    // Every libm name codegen can emit: the direct calls in
    // `codegen::method_call`'s `libm_sym` table, plus the LLVM intrinsics that
    // lower to a libm call on this target, plus `sqrt`/`abs`/`%`, which are
    // not in the float-math table but reach the same symbols. Each is declared
    // with its REAL signature — an approximate one would trip
    // `clashing_extern_declarations` against the fixture above, which declares
    // `cbrt`/`cbrtf` for the same reason and must agree.
    let probes = probe!(
        sin(f64) -> f64, sinf(f32) -> f32,
        cos(f64) -> f64, cosf(f32) -> f32,
        tan(f64) -> f64, tanf(f32) -> f32,
        asin(f64) -> f64, asinf(f32) -> f32,
        acos(f64) -> f64, acosf(f32) -> f32,
        atan(f64) -> f64, atanf(f32) -> f32,
        atan2(f64, f64) -> f64, atan2f(f32, f32) -> f32,
        sinh(f64) -> f64, sinhf(f32) -> f32,
        cosh(f64) -> f64, coshf(f32) -> f32,
        tanh(f64) -> f64, tanhf(f32) -> f32,
        asinh(f64) -> f64, asinhf(f32) -> f32,
        acosh(f64) -> f64, acoshf(f32) -> f32,
        atanh(f64) -> f64, atanhf(f32) -> f32,
        exp(f64) -> f64, expf(f32) -> f32,
        exp2(f64) -> f64, exp2f(f32) -> f32,
        expm1(f64) -> f64, expm1f(f32) -> f32,
        log(f64) -> f64, logf(f32) -> f32,
        log2(f64) -> f64, log2f(f32) -> f32,
        log10(f64) -> f64, log10f(f32) -> f32,
        log1p(f64) -> f64, log1pf(f32) -> f32,
        pow(f64, f64) -> f64, powf(f32, f32) -> f32,
        sqrt(f64) -> f64, sqrtf(f32) -> f32,
        cbrt(f64) -> f64, cbrtf(f32) -> f32,
        hypot(f64, f64) -> f64, hypotf(f32, f32) -> f32,
        fabs(f64) -> f64, fabsf(f32) -> f32,
        fmod(f64, f64) -> f64, fmodf(f32, f32) -> f32,
        fma(f64, f64, f64) -> f64, fmaf(f32, f32, f32) -> f32,
        floor(f64) -> f64, floorf(f32) -> f32,
        ceil(f64) -> f64, ceilf(f32) -> f32,
        round(f64) -> f64, roundf(f32) -> f32,
        trunc(f64) -> f64, truncf(f32) -> f32,
        copysign(f64, f64) -> f64, copysignf(f32, f32) -> f32,
    );

    /// Class (a): IEEE 754 pins the result, so a shadowing implementation
    /// cannot answer differently. Everything else on the list is
    /// transcendental — correctly-rounded results are not required and no two
    /// libms deliver them identically.
    const IEEE_EXACT: &[&str] = &[
        "sqrt",
        "sqrtf",
        "fabs",
        "fabsf",
        "fmod",
        "fmodf",
        "fma",
        "fmaf",
        "floor",
        "floorf",
        "ceil",
        "ceilf",
        "round",
        "roundf",
        "trunc",
        "truncf",
        "copysign",
        "copysignf",
    ];

    let published = karac::codegen::PUBLISHED_LIBM_SYMBOLS;
    let mut unguarded: Vec<&str> = Vec::new();
    let mut shadowed = 0usize;
    for (name, statically, via_dlsym) in &probes {
        if *via_dlsym == 0 || *via_dlsym == *statically {
            continue;
        }
        shadowed += 1;
        if !IEEE_EXACT.contains(name) && !published.contains(name) {
            unguarded.push(name);
        }
    }
    assert!(
        unguarded.is_empty(),
        "these libm symbols resolve to one implementation in a Rust link and \
         another through `dlsym`, so the JIT lane calls different code from \
         `karac build` and `karac run --interp`: {unguarded:?}. Either the \
         result is exactly specified by IEEE 754 (add it to IEEE_EXACT with \
         the reason) or the symbol must be republished in \
         `LLJITEngine::define_compiler_rt_builtins` and listed in \
         PUBLISHED_LIBM_SYMBOLS, as `cbrt` is. {shadowed} of {} names are \
         shadowed on this host.",
        probes.len()
    );
}
