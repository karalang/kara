//! design.md §15 `extern "C"`: structs and scalars the C ABI places past
//! the plain registers, each against a C library `cc` builds, on every lane
//! (conformance/layout.py found them).
//!
//! - B-2026-11-22-53: a 16-byte-aligned struct (an `i128` field) passed or
//!   returned by value went through an 8-aligned copy: the compiled program
//!   misplaced the `byval` copy and C's aligned store through `sret`
//!   faulted.
//! - B-2026-11-22-54: the interpreter refused a seventh integer argument,
//!   a ninth float one and a 16-byte-aligned struct on the stack, and
//!   passed an `i128` as its low 64 bits.
//! - B-2026-11-22-55: `karac run` and `karac test` did not read
//!   `kara.toml`'s `[link]` table, so a package binding a C library ran
//!   only once built.

use std::path::{Path, PathBuf};
use std::process::Command;

static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A package in a fresh directory: `src/main.kara` is `main`, linked to
/// the library `cc` builds from `c`. `None` when there is no `cc`.
fn package(main: &str, c: &str) -> Option<PathBuf> {
    let dir = std::env::temp_dir().join(format!(
        "karac-ffi-abi-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/main.kara"), main).unwrap();
    std::fs::write(dir.join("lib.c"), c).unwrap();
    let lib = if cfg!(target_os = "macos") {
        "libabi.dylib"
    } else {
        "libabi.so"
    };
    let cc = Command::new("cc")
        .current_dir(&dir)
        .args(["-O1", "-shared", "-fPIC", "-o", lib, "lib.c"])
        .output()
        .ok()?;
    assert!(
        cc.status.success(),
        "{}",
        String::from_utf8_lossy(&cc.stderr)
    );
    std::fs::write(
        dir.join("kara.toml"),
        format!(
            "[package]\nname = \"abi\"\nversion = \"0.1.0\"\n\n[link]\nlibs = [\"abi\"]\nsearch-paths = [\"{}\"]\n",
            dir.display()
        ),
    )
    .unwrap();
    Some(dir)
}

fn karac(dir: &Path, args: &[&str]) -> (String, String, Option<i32>) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .env("LD_LIBRARY_PATH", dir)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

/// `main` on the interpreter, through `karac run` (the JIT under
/// `--features llvm`), and built: each must print `want`.
fn every_lane(main: &str, c: &str, want: &str) {
    let Some(dir) = package(main, c) else {
        eprintln!("skip: no `cc`");
        return;
    };
    let lanes: Vec<Vec<&str>> = if cfg!(feature = "llvm") {
        vec![
            vec!["__mir-run", "src/main.kara"],
            vec!["run", "--interp", "src/main.kara"],
            vec!["run", "src/main.kara"],
        ]
    } else {
        vec![
            vec!["__mir-run", "src/main.kara"],
            vec!["run", "src/main.kara"],
        ]
    };
    for lane in lanes {
        let (out, err, code) = karac(&dir, &lane);
        assert_eq!(out, want, "{lane:?} (exit {code:?}): {err}");
    }
    if cfg!(feature = "llvm") {
        let (_, err, code) = karac(&dir, &["build"]);
        assert_eq!(code, Some(0), "build: {err}");
        let run = Command::new(dir.join("dist/native/abi"))
            .env("LD_LIBRARY_PATH", &dir)
            .env("DYLD_LIBRARY_PATH", &dir)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            want,
            "built: {}",
            String::from_utf8_lossy(&run.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ffi_abi_run_reads_the_link_table() {
    every_lane(
        "unsafe extern \"C\" { fn twice(x: i64) -> i64; }\nfn main() { println(twice(21)); }\n",
        "#include <stdint.h>\nint64_t twice(int64_t x) { return 2 * x; }\n",
        "42\n",
    );
}

#[test]
fn ffi_abi_more_arguments_than_registers() {
    every_lane(
        "unsafe extern \"C\" {\n\
         \x20   fn ints(a: i64, b: i32, c: u8, d: i16, e: u64, f: i64, g: i32, h: u16) -> i64;\n\
         \x20   fn floats(a: f64, b: f32, c: f64, d: f64, e: f64, f: f64, g: f64, h: f64, i: f32, j: f64, k: i64) -> f64;\n\
         }\n\
         fn main() {\n\
         \x20   println(ints(1, 2, 3, 4, 5, 6, 7, 8));\n\
         \x20   println(floats(1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.5, 10.25, 3));\n\
         }\n",
        "#include <stdint.h>\n\
         int64_t ints(int64_t a, int32_t b, uint8_t c, int16_t d, uint64_t e, int64_t f, int32_t g, uint16_t h) {\n\
         \x20   return a + 10 * b + 100 * c + 1000 * d + 10000 * e + 100000 * f + 1000000 * g + 10000000 * h;\n\
         }\n\
         double floats(double a, float b, double c, double d, double e, double f, double g, double h, float i, double j, int64_t k) {\n\
         \x20   return a + b + c + d + e + f + g + h + 100 * i + 1000 * j + k;\n\
         }\n",
        "87654321\n11239.0\n",
    );
}

/// The `i128` cases: two integer registers, or the stack whole when only
/// one is left (and the next integer argument takes that one); a struct
/// aligned to 16 by its `i128` in registers' place, as a `byval` copy and
/// through `sret`. x86-64 Linux only: the interpreter passes a 128-bit
/// integer only on x86-64, and the system `cc` there is the ABI's GCC.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn ffi_abi_i128_and_sixteen_byte_aligned_structs() {
    every_lane(
        "#[repr(C)]\n#[derive(Copy, Clone)]\n\
         struct W { a: i32, b: i128, c: f32, d: i32 }\n\
         unsafe extern \"C\" {\n\
         \x20   fn wide(a: i32, b: u64, c: u16, d: u16, e: i64, f: i128, g: char, h: i16) -> i64;\n\
         \x20   fn neg(x: i128) -> i128;\n\
         \x20   fn make(x: i64) -> W;\n\
         \x20   fn take(a: i64, w: W, b: i64) -> i64;\n\
         \x20   fn at(p: *const W) -> i64;\n\
         }\n\
         fn main() {\n\
         \x20   println(wide(1, 2, 3, 4, 5, 6, 'x', 8));\n\
         \x20   println(neg(170141183460469231731687303715884105727));\n\
         \x20   let w = make(7);\n\
         \x20   println(f\"{w.a} {w.b} {w.c} {w.d}\");\n\
         \x20   println(take(1, w, 2));\n\
         \x20   println(at(ptr.const(w)));\n\
         }\n",
        "#include <stdint.h>\n\
         struct W { int32_t a; __int128 b; float c; int32_t d; };\n\
         int64_t wide(int32_t a, uint64_t b, uint16_t c, uint16_t d, int64_t e, __int128 f, uint32_t g, int16_t h) {\n\
         \x20   return a + 10 * b + 100 * c + 1000 * d + 10000 * e + 100000 * (int64_t)f + 1000000 * (int64_t)g + 1000000000LL * h;\n\
         }\n\
         __int128 neg(__int128 x) { return -x; }\n\
         struct W make(int64_t x) { struct W w = { (int32_t)x, ((__int128)x << 80) + 1, 2.5f, -3 }; return w; }\n\
         int64_t take(int64_t a, struct W w, int64_t b) { return a + w.a * 10 + (int64_t)(w.b >> 80) * 100 + (int64_t)w.c * 1000 + w.d * 10000 + b * 100000; }\n\
         int64_t at(const struct W* p) { return p->a + (int64_t)(p->b >> 80) * 10; }\n",
        "8120654321\n-170141183460469231731687303715884105727\n\
         7 8462480737302404222943233 2.5 -3\n172771\n77\n",
    );
}

/// B-2026-11-27-62: a struct the registers no longer hold goes on the
/// stack whole, and the compiled call site carries `byval`, so its copy is
/// filled (it reached C as zeros); B-2026-11-27-53: the interpreter's
/// slots sit at the struct's alignment, which C reports back.
#[test]
fn ffi_abi_structs_on_the_stack_after_the_registers() {
    every_lane(
        "#[repr(C)]\n#[derive(Copy, Clone)]\n\
         struct A { x: u16, y: f32 }\n\
         #[repr(C)]\n#[derive(Copy, Clone)]\n\
         struct B { a: i64, b: i64, c: i64 }\n\
         unsafe extern \"C\" {\n\
         \x20   fn ska(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64, a5: i64, a6: i64, s: A) -> i64;\n\
         \x20   fn skb(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64, a5: i64, a6: i64, s: B) -> i64;\n\
         \x20   fn skb6(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64, a5: i64, s: B) -> i64;\n\
         \x20   fn misaligned(p: *const B) -> i64;\n\
         }\n\
         fn main() {\n\
         \x20   println(ska(1, 2, 3, 4, 5, 6, 7, A { x: 3, y: 4.0 }));\n\
         \x20   println(skb(1, 2, 3, 4, 5, 6, 7, B { a: 1, b: 2, c: 3 }));\n\
         \x20   let b = B { a: 4, b: 5, c: 6 };\n\
         \x20   println(skb6(1, 2, 3, 4, 5, 6, b));\n\
         \x20   println(misaligned(ptr.const(b)));\n\
         }\n",
        "#include <stdint.h>\n\
         struct A { uint16_t x; float y; };\n\
         struct B { int64_t a, b, c; };\n\
         int64_t ska(int64_t a0, int64_t a1, int64_t a2, int64_t a3, int64_t a4, int64_t a5, int64_t a6, struct A s) { return a0 + a5 + a6 * 1000 + s.x * 10 + (int64_t)s.y; }\n\
         int64_t skb(int64_t a0, int64_t a1, int64_t a2, int64_t a3, int64_t a4, int64_t a5, int64_t a6, struct B s) { return a6 * 1000 + s.a * 100 + s.b * 10 + s.c; }\n\
         int64_t skb6(int64_t a0, int64_t a1, int64_t a2, int64_t a3, int64_t a4, int64_t a5, struct B s) { return a5 * 1000 + s.a * 100 + s.b * 10 + s.c; }\n\
         int64_t misaligned(const struct B* p) { return (int64_t)((uintptr_t)p % _Alignof(struct B)); }\n",
        "7041\n7123\n6456\n0\n",
    );
}

/// B-2026-11-22-56: a `union` value is refused naming the deferral, not
/// as "no definition for type".
#[test]
fn ffi_abi_union_use_names_the_deferral() {
    let dir = std::env::temp_dir().join(format!("karac-ffi-union-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("u.kara"),
        "#[repr(C)]\nunion U { a: u8, b: u64 }\nfn main() { let u = U { b: 5 }; println(U.size_of()); }\n",
    )
    .unwrap();
    let (_, err, code) = karac(&dir, &["check", "u.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(code, Some(1), "{err}");
    assert!(
        err.contains("`U`, a `union` (FFI unions are deferred to the systems track"),
        "{err}"
    );
    assert!(!err.contains("no definition for type"), "{err}");
}
