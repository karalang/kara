//! The probe's §15 FFI rows against glibc, pinned in one family: each cell
//! is the probe's program (or one beside it), run on the MIR interpreter
//! and, with `--features llvm`, compiled, and the two must print the same.
//!
//! - FF1 (B-2026-11-15-57): an `extern "C"` `cbrt` answers as the compiled program's.
//! - FF2 (B-2026-11-15-58): `#[kara_name("CFile")] type FILE;` is `CFile`; `ptr.is_null`
//!   takes a `*mut T`; `Str` / `String` have `as_ptr()`.
//! - FF3 (B-2026-11-15-59): a `#[repr(C)]` struct, a `#[repr(transparent)]` wrapper and
//!   a `distinct type` cross an `extern "C"` signature by value.
//! - FF4 (B-2026-11-15-60): `mem.zeroed()` under `T: Pod`.
//! - FF5 (B-2026-11-15-61): a pointer to a `#[repr(C)]` struct crosses on the
//!   interpreter.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("karac-probe-ffi-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn karac(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .unwrap()
}

fn backends() -> &'static [&'static str] {
    if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    }
}

/// Runs `file` on every backend and requires `want` from each. The
/// programs call a Unix libc (glibc on Linux), so off Unix this is a named
/// skip: the interpreter's `extern "C"` runs there only compiled, and the
/// programs' C types (`long`, `FILE` paths) are Unix's.
fn runs_as(file: &Path, want: &str) {
    if !cfg!(unix) {
        eprintln!(
            "SKIP {}: the probe's FFI programs need a Unix libc",
            file.display()
        );
        return;
    }
    for b in backends() {
        let out = karac(&[b, file.to_str().unwrap()]);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{b}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{b}");
    }
}

/// Writes `src` as a single file and runs it on every backend.
fn program_runs_as(tag: &str, src: &str, want: &str) {
    let dir = Scratch::new(tag);
    let file = dir.path().join("main.kara");
    std::fs::write(&file, src).unwrap();
    runs_as(&file, want);
}

/// `karac check` of `src` fails with `needle` among its errors.
fn refused(tag: &str, src: &str, needle: &str) {
    let dir = Scratch::new(tag);
    let file = dir.path().join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = karac(&["check", file.to_str().unwrap()]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{src}");
    assert!(err.contains(needle), "expected `{needle}` in:\n{err}");
}

// ── FF1 ─────────────────────────────────────────────────────────────

#[test]
fn ff1_an_extern_cbrt_answers_as_the_compiled_program() {
    program_runs_as(
        "ff1",
        r#"/// # Safety
/// libm, pure.
unsafe extern "C" {
    pure fn cbrt(x: f64) -> f64;
    pure fn cbrtf(x: f32) -> f32;
}
fn main() {
    println(cbrt(27.0));
    println(cbrt(27.0) == 27.0.cbrt());
    println(cbrtf(8.0f32));
}
"#,
        "3.0\ntrue\n2.0\n",
    );
}

// ── FF2 ─────────────────────────────────────────────────────────────

#[test]
fn ff2_an_opaque_type_is_its_kara_name() {
    let dir = Scratch::new("ff2-file");
    let data = dir.path().join("probe.txt");
    let src = r#"/// # Safety
/// glibc stdio with FILE handles.
unsafe extern "C" {
    #[kara_name("CFile")]
    type FILE;
    fn fopen(path: *const u8, mode: *const u8) -> *mut CFile with reads(FileSystem) writes(FileSystem);
    fn fputs(s: *const u8, f: *mut CFile) -> i32 with writes(FileSystem);
    fn fgets(buf: *mut u8, n: i32, f: *mut CFile) -> *mut u8 with reads(FileSystem);
    fn fclose(f: *mut CFile) -> i32 with writes(FileSystem);
    fn ftell(f: *mut CFile) -> i64 with reads(FileSystem);
}
fn main() {
    let f = fopen(c"@PATH@".as_ptr(), c"w".as_ptr());
    println(ptr.is_null(f));
    fputs(c"hello\n".as_ptr(), f);
    println(f"wrote {ftell(f)}");
    fclose(f);
    let g = fopen(c"@PATH@".as_ptr(), c"r".as_ptr());
    let mut buf: Array[u8, 16] = [0u8; 16];
    fgets(ptr.mut(buf[0]), 16, g);
    fclose(g);
    let mut s = String.new();
    for b in buf.iter() { if b == 10u8 or b == 0u8 { break; } s.push(b as char); }
    println(f"read {s}");
    let none = fopen(c"/nonexistent/kara".as_ptr(), c"r".as_ptr());
    println(none.is_null());
}
"#
    .replace("@PATH@", data.to_str().unwrap());
    let file = dir.path().join("main.kara");
    std::fs::write(&file, src).unwrap();
    runs_as(&file, "false\nwrote 6\nread hello\ntrue\n");
}

#[test]
fn ff2_the_foreign_name_of_a_rebound_type_is_not_a_kara_name() {
    refused(
        "ff2-foreign",
        r#"unsafe extern "C" {
    #[kara_name("CFile")]
    type FILE;
    fn fclose(f: *mut FILE) -> i32;
}
fn main() { println(1); }
"#,
        "undefined type 'FILE'",
    );
}

#[test]
fn ff2_is_null_takes_either_raw_pointer() {
    program_runs_as(
        "ff2-null",
        "fn main() {
    let mut x = 1;
    let p: *mut i64 = ptr.mut(x);
    println(ptr.is_null(p));
    let q: *mut i64 = ptr.null_mut();
    println(ptr.is_null(q));
    println(q.is_null());
    let r: *const i64 = ptr.null();
    println(ptr.is_null(r));
    println(ptr.addr(q) == 0);
}
",
        "false\ntrue\ntrue\ntrue\ntrue\n",
    );
    refused(
        "ff2-null-int",
        "fn main() { println(ptr.is_null(3)); }\n",
        "'ptr.is_null' expects a raw pointer",
    );
}

#[test]
fn ff2_a_string_lends_c_its_bytes() {
    program_runs_as(
        "ff2-as-ptr",
        r#"/// # Safety
/// libc, reading `n` bytes of each.
unsafe extern "C" {
    pure fn strncmp(a: *const u8, b: *const u8, n: usize) -> i32;
    pure fn memchr(s: *const u8, c: i32, n: usize) -> *const u8;
}
fn main() {
    let s: Str = "hello world";
    let t = String.from("hello there");
    println(strncmp(s.as_ptr(), t.as_ptr(), 6));
    println(strncmp(s.as_ptr(), t.as_ptr(), 7) > 0);
    let at = memchr(t.as_ptr(), 116, t.len() as usize);
    println(ptr.addr(at) - ptr.addr(t.as_ptr()));
    let p = t.as_ptr();
    println(unsafe { p.read() });
}
"#,
        "0\ntrue\n6\n104\n",
    );
}

// ── FF3 ─────────────────────────────────────────────────────────────

#[test]
fn ff3_glibc_returns_a_struct_by_value() {
    program_runs_as(
        "ff3-div",
        r#"#[repr(C)]
struct DivT { quot: i32, rem: i32 }
#[repr(C)]
struct LDivT { quot: i64, rem: i64 }
/// # Safety
/// glibc stdlib div/ldiv return structs by value.
unsafe extern "C" {
    pure fn div(a: i32, b: i32) -> DivT;
    pure fn ldiv(a: i64, b: i64) -> LDivT;
}
fn main() {
    let d = div(10, 3);
    println(f"{d.quot} {d.rem}");
    let l = ldiv(-22, -3);
    println(f"{l.quot} {l.rem}");
}
"#,
        "3 1\n7 -1\n",
    );
}

#[test]
fn ff3_a_transparent_wrapper_crosses_as_its_field() {
    program_runs_as(
        "ff3-transparent",
        r#"#[repr(transparent)]
struct Fd { raw: i64 }
#[repr(transparent)]
distinct type Len = i64;
/// # Safety
/// labs from libc, called with the wrappers.
unsafe extern "C" {
    pure fn labs(x: Fd) -> Fd;
    pure fn llabs(x: Len) -> Len;
}
fn main() {
    println(f"{Fd.size_of()} {Fd.align_of()}");
    println(labs(Fd { raw: -42 }).raw);
    println(llabs(Len(-7)).raw());
}
"#,
        "8 8\n42\n7\n",
    );
}

/// The C library [`ff3_structs_cross_by_the_platform_abi`] links: a struct
/// of each register class, one that crosses in memory, and arguments past
/// the registers.
const KSTRUCT_C: &str = r#"#include <stdint.h>
typedef struct { double x, y; } Pt;
typedef struct { int64_t a; double b; } Mix;
typedef struct { int64_t a, b, c; } Big;
typedef struct { float a, b, c; } F3;
typedef struct { uint8_t tag; int32_t v; } Small;
Pt pt_scale(Pt p, double k) { Pt r = { p.x * k, p.y * k }; return r; }
Mix mix_step(Mix m) { Mix r = { m.a + 1, m.b * 2.0 }; return r; }
Big big_sum(Big b) { Big r = { b.a + b.b + b.c, b.b * 10, b.c * 100 }; return r; }
F3 f3_add(F3 a, F3 b) { F3 r = { a.a + b.a, a.b + b.b, a.c + b.c }; return r; }
int32_t small_get(Small s) { return s.tag == 7 ? s.v : -1; }
void big_fill(Big *out) { out->a = 1; out->b = 2; out->c = 3; }
int64_t many(int64_t a, int64_t b, int64_t c, Pt p, Mix m) {
    return a + b + c + (int64_t)p.x + (int64_t)p.y + m.a + (int64_t)m.b;
}
int64_t past(int64_t a, int64_t b, int64_t c, int64_t d, int64_t e, int64_t f, Mix m) {
    return a + b + c + d + e + f + m.a + (int64_t)m.b;
}
Big big_after(int64_t a, int64_t b, int64_t c, int64_t d, int64_t e, Big x) {
    Big r = { a + b + c + d + e, x.b, x.a + x.c }; return r;
}
"#;

const KSTRUCT_KARA: &str = r#"#[repr(C)]
struct Pt { x: f64, y: f64 }
#[repr(C)]
struct Mix { a: i64, b: f64 }
#[repr(C)]
struct Big { a: i64, b: i64, c: i64 }
#[repr(C)]
struct F3 { a: f32, b: f32, c: f32 }
#[repr(C)]
struct Small { tag: u8, v: i32 }
/// # Safety
/// Plain values, and a pointer to a local `Big`.
unsafe extern "C" {
    pure fn pt_scale(p: Pt, k: f64) -> Pt;
    pure fn mix_step(m: Mix) -> Mix;
    pure fn big_sum(b: Big) -> Big;
    pure fn f3_add(a: F3, b: F3) -> F3;
    pure fn small_get(s: Small) -> i32;
    fn big_fill(out: *mut Big);
    pure fn many(a: i64, b: i64, c: i64, p: Pt, m: Mix) -> i64;
    pure fn past(a: i64, b: i64, c: i64, d: i64, e: i64, f: i64, m: Mix) -> i64;
    pure fn big_after(a: i64, b: i64, c: i64, d: i64, e: i64, x: Big) -> Big;
}
fn main() {
    let p = pt_scale(Pt { x: 1.5, y: -2.0 }, 4.0);
    println(f"{p.x} {p.y}");
    let m = mix_step(Mix { a: 41, b: 1.25 });
    println(f"{m.a} {m.b}");
    let b = big_sum(Big { a: 1, b: 2, c: 3 });
    println(f"{b.a} {b.b} {b.c}");
    let f = f3_add(F3 { a: 0.5, b: 1.5, c: 2.5 }, F3 { a: 1.0, b: 2.0, c: 3.0 });
    println(f"{f.a} {f.b} {f.c}");
    println(small_get(Small { tag: 7, v: 99 }));
    let mut out = Big { a: 0, b: 0, c: 0 };
    big_fill(ptr.mut(out));
    println(f"{out.a} {out.b} {out.c}");
    println(many(1, 2, 3, Pt { x: 4.0, y: 5.0 }, Mix { a: 6, b: 7.0 }));
    println(past(1, 2, 3, 4, 5, 6, Mix { a: 7, b: 8.0 }));
    let r = big_after(1, 2, 3, 4, 5, Big { a: 10, b: 20, c: 30 });
    println(f"{r.a} {r.b} {r.c}");
}
"#;

#[test]
fn ff3_structs_cross_by_the_platform_abi() {
    if !cfg!(unix) || !(cfg!(target_arch = "x86_64") || cfg!(target_arch = "aarch64")) {
        return;
    }
    let dir = Scratch::new("ff3-abi");
    let c = dir.path().join("kstruct.c");
    std::fs::write(&c, KSTRUCT_C).unwrap();
    let (lib, flag) = if cfg!(target_os = "macos") {
        ("libkstruct.dylib", "-dynamiclib")
    } else {
        ("libkstruct.so", "-shared")
    };
    let built = Command::new("cc")
        .args([flag, "-fPIC", "-O1", "-o"])
        .arg(dir.path().join(lib))
        .arg(&c)
        .output()
        .expect("a C compiler (`cc`) to build the test library");
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    std::fs::write(
        dir.path().join("kara.toml"),
        format!(
            "[package]\nname = \"kstruct\"\n\n[link]\nlibs = [\"kstruct\"]\nsearch-paths = [\"{}\"]\n",
            dir.path().display()
        ),
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    let main = dir.path().join("src/main.kara");
    std::fs::write(&main, KSTRUCT_KARA).unwrap();
    runs_as(
        &main,
        "6.0 -8.0\n42 2.5\n6 20 300\n1.5 3.5 5.5\n99\n1 2 3\n28\n36\n15 20 40\n",
    );
}

// ── FF4 ─────────────────────────────────────────────────────────────

#[test]
fn ff4_mem_zeroed_makes_the_all_zeros_pod_value() {
    program_runs_as(
        "ff4",
        "import std.mem;
#[derive(Pod)]
#[repr(C)]
struct P { a: i64, b: bool, c: f64 }
#[derive(Pod)]
struct Q { p: P, t: (u8, f32), arr: Array[u16, 3] }
fn main() {
    let p: P = mem.zeroed();
    println(f\"{p.a} {p.b} {p.c}\");
    let q: Q = mem.zeroed();
    println(f\"{q.p.a} {q.t.0} {q.t.1} {q.arr[2]}\");
    let buf: Array[u8, 4096] = mem.zeroed();
    println(buf[4095]);
    let n: u32 = mem.zeroed();
    println(n);
}
",
        "0 false 0.0\n0 0 0.0 0\n0\n0\n",
    );
}

#[test]
fn ff4_a_type_whose_zero_bits_are_not_a_value_is_not_pod() {
    refused(
        "ff4-ref",
        "fn main() { let r: Option[ref i64] = mem.zeroed(); println(r.is_none()); }\n",
        "`Option[ref i64]` does not implement `Pod`",
    );
    refused(
        "ff4-string",
        "#[derive(Pod)]\nstruct S { s: String }\nfn main() { println(1); }\n",
        "struct 'S' derives Pod but field 's' has non-Pod type 'String'",
    );
    refused(
        "ff4-enum",
        "#[derive(Pod)]\nenum E { A, B }\nfn main() { println(1); }\n",
        "enum 'E' cannot derive Pod",
    );
    refused(
        "ff4-underived",
        "struct S { a: i64 }\nfn main() { let s: S = mem.zeroed(); println(s.a); }\n",
        "`S` does not implement `Pod`",
    );
}

// ── FF5 ─────────────────────────────────────────────────────────────

#[test]
fn ff5_a_pointer_to_a_repr_c_struct_crosses_and_is_read_back() {
    program_runs_as(
        "ff5",
        r#"#[repr(C)]
struct Tm { sec: i32, min: i32, hour: i32, mday: i32, mon: i32, year: i32, wday: i32, yday: i32, isdst: i32, gmtoff: i64, zone: *const u8 }
/// # Safety
/// glibc time conversion.
unsafe extern "C" {
    fn gmtime_r(t: *const i64, out: *mut Tm) -> *mut Tm with reads(Clock);
    fn timegm(tm: *mut Tm) -> i64 with reads(Clock);
}
fn main() {
    let t: i64 = 1700000000;
    let mut tm: Tm = Tm { sec: 0, min: 0, hour: 0, mday: 0, mon: 0, year: 0, wday: 0, yday: 0, isdst: 0, gmtoff: 0, zone: ptr.null() };
    gmtime_r(ptr.const(t), ptr.mut(tm));
    println(f"{tm.year} {tm.mon} {tm.mday} {tm.hour} {tm.min} {tm.sec} {tm.wday}");
    println(ptr.is_null(tm.zone));
    println(timegm(ptr.mut(tm)));
}
"#,
        "123 10 14 22 13 20 2\nfalse\n1700000000\n",
    );
}
