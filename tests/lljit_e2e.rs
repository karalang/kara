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
