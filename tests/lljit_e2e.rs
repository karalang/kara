//! Phase-7 L560 W3.1: JIT-based E2E test harness with stdout capture.
//!
//! Mirrors the shape of `tests/codegen.rs::codegen_tests::run_program`
//! but routes through `LLJITEngine` instead of the AOT path (object
//! file, link, spawn subprocess). The JIT runs `main` in this process,
//! so we redirect fd 1 around the call to capture printf output and
//! restore on the way out.
//!
//! W3.1 acceptance: a representative subset of the existing codegen
//! E2E tests (println int / bool / Vec sum / Map insert+get / `?` on
//! Result / par-block) round-trips through this harness with output
//! matching the AOT path. If ≥80% pass, the JIT path is real and we
//! can grind through the rest in W3.2+. If a category fails for a
//! structural reason (e.g., par-block thread lifecycle clashes with
//! engine Drop), that's a real W3+ design item to address.

#![cfg(feature = "lljit_prototype")]

use std::io::{Read, Seek, SeekFrom};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};

use karac::codegen::{compile_to_ir, LLJITEngine};

// `karac_runtime::__preserve_no_mangle_symbols` (see
// `runtime/src/lib.rs`) holds each `#[no_mangle]` symbol live via
// `black_box` so rlib-level DCE can't drop them — that's what makes
// `dlsym(RTLD_DEFAULT, ...)` (the LLJIT process-symbol-search
// generator's lookup mechanism) succeed for `karac_*` runtime
// symbols at JIT-link time. Called once at module init via a static
// initializer pattern; the `#[used]` static below is what guarantees
// the call site itself isn't optimized out.
fn force_link_karac_runtime() -> usize {
    karac_runtime::__preserve_no_mangle_symbols()
}

#[used]
static _FORCE_LINK_CALL_SITE: fn() -> usize = force_link_karac_runtime;

// ── KARAC_SPAWN_SITES test-binary stand-ins ──────────────────────────
// In AOT builds, codegen emits these globals into the user program's
// LLVM module; the runtime's `extern KARAC_SPAWN_SITES*` declarations
// (`runtime/src/lib.rs` ~L1059, `#[cfg(not(test))]`-gated) resolve
// against them at link time. In the LLJIT integration tests, codegen
// emits them into each JITted module (visible only inside that
// module's JITDylib), so the test binary's static link of the runtime
// rlib has no satisfier for these references.
//
// We provide neutral stand-ins here:
//   - `_ENABLED = 0` so `karac_runtime_has_debug_metadata` returns
//     false and the slice-4/5 introspection paths short-circuit;
//   - `_LEN = 0` so any iteration over the table is a no-op;
//   - `KARAC_SPAWN_SITES` is a 32-byte zero placeholder (alignment 8
//     to match `KaracSpawnSiteEntry`'s pointer-field alignment).
// JITted user code reads its OWN KARAC_SPAWN_SITES from its module's
// definitions — these stand-ins only satisfy the test binary's runtime
// link, not the user program's runtime behavior.
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

/// JIT-route a Kāra program through `LLJITEngine` and capture its
/// stdout. Mirrors `tests/codegen.rs::codegen_tests::run_program`'s
/// return type (`Option<String>`) so individual tests adopting this
/// harness keep the same shape.
///
/// Returns `Some(stdout)` if the JIT compiles + executes; `None` is
/// reserved for environments where the JIT can't initialize (none
/// expected on the host platforms we care about, but matching the
/// AOT helper's `Option` shape).
fn jit_run_program(src: &str) -> Option<String> {
    // Belt-and-suspenders: the `#[used]` static above pins the call
    // site at link time, and this runtime call ensures the function
    // body's symbol references are evaluated (not const-folded away).
    let _ = force_link_karac_runtime();
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

    let engine = LLJITEngine::new().ok()?;
    engine.add_ir_module(&ir).expect("add_ir_module");
    let addr = engine.lookup_address("main").expect("lookup main");

    // dup2-based stdout redirect to a temp file. A tempfile (not a
    // pipe) sidesteps buffer-fill blocking for programs that print
    // more than a pipe buffer's worth — at the cost of disk IO, which
    // is acceptable for tests. Order:
    //   1. Force-flush whatever the host process's stdout has buffered
    //      so it doesn't leak into our captured stream.
    //   2. Save fd 1 via dup, redirect 1 → tempfile.
    //   3. Call JIT'd main.
    //   4. Force-flush JIT'd stdio (printf is line-buffered on TTYs and
    //      fully-buffered when redirected; we want all of it).
    //   5. Restore fd 1 from the saved dup, close + drop the saved fd.
    //   6. Rewind tempfile to start, read.
    // exit code is intentionally discarded — the AOT harness's
    // run_program also returns stdout only. Programs that intend to
    // fail via exit code are out of scope for W3.1's representative
    // subset.
    let captured = unsafe {
        libc::fflush(std::ptr::null_mut());
        let saved_stdout = libc::dup(1);
        assert!(saved_stdout >= 0, "dup(stdout) failed");
        let mut tmpfile = tempfile().expect("create tempfile");
        let rc = libc::dup2(tmpfile.as_raw_fd(), 1);
        assert!(rc >= 0, "dup2 failed");

        type MainFn = unsafe extern "C" fn() -> i32;
        let main_fn: MainFn = std::mem::transmute(addr as usize);
        let _exit = main_fn();

        libc::fflush(std::ptr::null_mut());
        let rc = libc::dup2(saved_stdout, 1);
        assert!(rc >= 0, "dup2 restore failed");
        libc::close(saved_stdout);

        tmpfile.seek(SeekFrom::Start(0)).expect("seek");
        let mut out = String::new();
        tmpfile.read_to_string(&mut out).expect("read");
        out
    };

    Some(captured)
}

/// Captured stdout + the JIT'd `main`'s C-ABI exit code. Mirrors what
/// the AOT path's `Output` exposes via `Command::output()`; tests that
/// need to assert on non-zero exit codes (panics, error returns, etc.)
/// use this variant instead of `jit_run_program`. W3.2c.
fn jit_run_program_capturing(src: &str) -> Option<(String, i32)> {
    let _ = force_link_karac_runtime();
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

    let engine = LLJITEngine::new().ok()?;
    engine.add_ir_module(&ir).expect("add_ir_module");
    let addr = engine.lookup_address("main").expect("lookup main");

    let (captured, exit_code) = unsafe {
        libc::fflush(std::ptr::null_mut());
        let saved_stdout = libc::dup(1);
        assert!(saved_stdout >= 0, "dup(stdout) failed");
        let mut tmpfile = tempfile().expect("create tempfile");
        let rc = libc::dup2(tmpfile.as_raw_fd(), 1);
        assert!(rc >= 0, "dup2 failed");

        type MainFn = unsafe extern "C" fn() -> i32;
        let main_fn: MainFn = std::mem::transmute(addr as usize);
        let exit = main_fn();

        libc::fflush(std::ptr::null_mut());
        let rc = libc::dup2(saved_stdout, 1);
        assert!(rc >= 0, "dup2 restore failed");
        libc::close(saved_stdout);

        tmpfile.seek(SeekFrom::Start(0)).expect("seek");
        let mut out = String::new();
        tmpfile.read_to_string(&mut out).expect("read");
        (out, exit)
    };

    Some((captured, exit_code))
}

/// Create a fresh unnamed temp file (O_RDWR). Stays open via the
/// returned `std::fs::File`; unlinks on close (mkstemp + unlink).
fn tempfile() -> std::io::Result<std::fs::File> {
    use std::ffi::CString;
    let template = CString::new("/tmp/karac_jit_e2e_XXXXXX").unwrap();
    let mut bytes = template.into_bytes_with_nul();
    let fd = unsafe { libc::mkstemp(bytes.as_mut_ptr() as *mut libc::c_char) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // Immediately unlink so the inode goes away when the fd closes.
    let path = std::ffi::CStr::from_bytes_with_nul(&bytes).unwrap();
    unsafe {
        libc::unlink(path.as_ptr());
    }
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    Ok(std::fs::File::from(owned))
}

// Bind `OwnedFd::into_raw_fd` so the import isn't dead.
#[allow(dead_code)]
fn _suppress_into_raw_fd_unused(fd: OwnedFd) -> i32 {
    fd.into_raw_fd()
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

// ── W3.2 surface ─────────────────────────────────────────────────────
// par-blocks, `?` on Result, and other surface that depends on runtime
// symbols beyond the libc/Vec/Map base. The W3.2a finding (KARAC_SPAWN_SITES
// stand-ins above) was needed before this could link at all.

#[test]
fn jit_e2e_question_mark_happy_path() {
    // `?` propagates an Ok through to the surrounding Result. Happy
    // path: `add_ten(true)` returns Ok(52), main prints 52. Exercises
    // codegen's `?` lowering + the runtime's karac_error_trace_clear
    // at startup (which the force-link list covers).
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
    // stderr. Stdout only carries the println output from main.
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
    // explicitly. Sanity check the variant before pivoting to non-zero
    // exit code tests (which would need codegen to lower a non-zero
    // exit from a top-level `Err(_)` — out of scope for W3.2, but the
    // capturing variant is the right shape for when that lands).
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
