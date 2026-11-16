//! design.md §15 conformance against a C compiler (review-thread request
//! 2026-11-16): `#[repr(C)]` structs laid out as `cc` lays them out, read
//! and written from both sides, and `#[repr]` C-like enums crossing by value
//! and inside structs. Each program runs on the MIR interpreter and, with
//! `--features llvm`, compiled, and both must print the same.
//!
//! - B-2026-11-16-233: a `#[repr(intN)]` / `#[repr(C)]` enum was one byte
//!   holding its variant index, so a struct holding one had the wrong layout
//!   (24 bytes in C, 16 in Kāra) and C read the index, not the value.
//! - B-2026-11-16-238: the interpreter passed C one element of an array of
//!   `#[repr(C)]` structs, so C indexing past the pointer read zeros.
//! - B-2026-11-16-236: a `// Safety:` comment with no word after it
//!   documented an `unsafe` block.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("karac-s15-pass-{tag}-{}", std::process::id()));
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

fn karac(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

/// The C library and its manifest in a scratch project, with `main` as
/// `src/main.kara`; `None` off x86-64 / AArch64 Unix.
fn project(tag: &str, main: &str) -> Option<Scratch> {
    if !cfg!(unix) || !(cfg!(target_arch = "x86_64") || cfg!(target_arch = "aarch64")) {
        return None;
    }
    let dir = Scratch::new(tag);
    let c = dir.path().join("lay.c");
    std::fs::write(&c, LAY_C).unwrap();
    let (lib, flag) = if cfg!(target_os = "macos") {
        ("liblay.dylib", "-dynamiclib")
    } else {
        ("liblay.so", "-shared")
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
            "[package]\nname = \"lay\"\n\n[link]\nlibs = [\"lay\"]\nsearch-paths = [\"{}\"]\n",
            dir.path().display()
        ),
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/main.kara"), main).unwrap();
    Some(dir)
}

fn runs_as(dir: &Path, want: &str) {
    let backends: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for b in backends {
        let out = karac(dir, &[b, "src/main.kara"]);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{b}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{b}");
    }
}

const LAY_C: &str = r##"#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
typedef struct { uint8_t a; uint16_t b; uint8_t c; } S1;
typedef struct { uint8_t a; double b; uint8_t c; } S2;
typedef struct { int32_t x; S1 in; int64_t y; } S3;
typedef struct { uint8_t a; uint32_t arr[3]; uint8_t z; } S4;
typedef struct { bool f; uint32_t ch; int16_t h; float g; int8_t i; } S5;
typedef struct { uint64_t a; uint8_t b; } S6;
typedef struct { uint8_t e8; int32_t e32; uint16_t e16; const void *p; } S7;
typedef struct { int32_t fd; uint8_t k; } S8;
typedef struct { uint8_t a; S2 s; uint16_t t[2]; } S9;
#define L(T) int64_t lay_size_##T(void){return sizeof(T);} int64_t lay_align_##T(void){return _Alignof(T);}
L(S1) L(S2) L(S3) L(S4) L(S5) L(S6) L(S7) L(S8) L(S9)
/* field values, read at C's offsets, packed into one checksum string of digits */
int64_t s1_read(const S1*p){return p->a*10000+p->b*10+p->c;}
int64_t s2_read(const S2*p){return (int64_t)(p->a*1000+p->b*10)+p->c;}
int64_t s3_read(const S3*p){return p->x*1000000LL + s1_read(&p->in)*10 + p->y;}
int64_t s4_read(const S4*p){return p->a*1000000LL+p->arr[0]*10000+p->arr[1]*100+p->arr[2]*10+p->z;}
int64_t s5_read(const S5*p){return (p->f?1:0)*100000000LL + p->ch*100000 + p->h*1000 + (int64_t)p->g*10 + p->i;}
int64_t s6_read(const S6*p){return p->a*10+p->b;}
int64_t s7_read(const S7*p){return p->e8*100000+p->e32*1000+p->e16*10+(p->p!=0);}
int64_t s8_read(const S8*p){return p->fd*10+p->k;}
int64_t s9_read(const S9*p){return p->a*100000000LL + s2_read(&p->s)*100 + p->t[0]*10+p->t[1];}
int64_t s1_arr_read(const S1*p, int64_t n){int64_t t=0; for(int64_t i=0;i<n;i++) t=t*100+p[i].c; return t;}
void s5_fill(S5*p){p->f=true; p->ch=0x1F600; p->h=-12; p->g=2.5f; p->i=-3;}
void s9_fill(S9*p){p->a=9; p->s.a=1; p->s.b=0.5; p->s.c=2; p->t[0]=300; p->t[1]=7;}
void s4_fill(S4*p){p->a=4; p->arr[0]=11; p->arr[1]=22; p->arr[2]=33; p->z=5;}
typedef enum { RED = -1, GREEN = 40, BLUE = 41 } Color;
Color next_color(Color c) { return c == RED ? GREEN : c == GREEN ? BLUE : RED; }
int32_t color_code(Color c) { return (int32_t)c * 2; }
typedef struct { uint8_t kind; Color c; uint16_t n; } Tagged;
Tagged tagged_bump(Tagged t) { t.c = next_color(t.c); t.n += 1; return t; }
void tagged_fill(Tagged *t) { t->kind = 3; t->c = BLUE; t->n = 77; }
uint8_t small_enum_twice(uint8_t e) { return e * 2; }
"##;

const LAY_KARA: &str = r##"#[repr(C)] struct S1 { a: u8, b: u16, c: u8 }
#[repr(C)] struct S2 { a: u8, b: f64, c: u8 }
#[repr(C)] struct S3 { x: i32, inner: S1, y: i64 }
#[repr(C)] struct S4 { a: u8, arr: Array[u32, 3], z: u8 }
#[repr(C)] struct S5 { f: bool, ch: u32, h: i16, g: f32, i: i8 }
#[repr(C)] struct S6 { a: u64, b: u8 }
#[repr(u8)] enum E8 { A = 1, B = 7 }
#[repr(i32)] enum E32 { X = -1, Y = 42 }
#[repr(u16)] enum E16 { P = 3, Q = 9 }
#[repr(C)] struct S7 { e8: E8, e32: E32, e16: E16, p: *const u8 }
#[repr(transparent)] distinct type Fd = i32;
#[repr(C)] struct S8 { fd: Fd, k: u8 }
#[repr(C)] struct S9 { a: u8, s: S2, t: Array[u16, 2] }
/// # Safety
/// liblay.so, built from lay.c beside this file.
unsafe extern "C" {
    pure fn lay_size_S1() -> i64; pure fn lay_align_S1() -> i64;
    pure fn lay_size_S2() -> i64; pure fn lay_align_S2() -> i64;
    pure fn lay_size_S3() -> i64; pure fn lay_align_S3() -> i64;
    pure fn lay_size_S4() -> i64; pure fn lay_align_S4() -> i64;
    pure fn lay_size_S5() -> i64; pure fn lay_align_S5() -> i64;
    pure fn lay_size_S6() -> i64; pure fn lay_align_S6() -> i64;
    pure fn lay_size_S7() -> i64; pure fn lay_align_S7() -> i64;
    pure fn lay_size_S8() -> i64; pure fn lay_align_S8() -> i64;
    pure fn lay_size_S9() -> i64; pure fn lay_align_S9() -> i64;
    fn s1_read(p: *const S1) -> i64;
    fn s2_read(p: *const S2) -> i64;
    fn s3_read(p: *const S3) -> i64;
    fn s4_read(p: *const S4) -> i64;
    fn s5_read(p: *const S5) -> i64;
    fn s6_read(p: *const S6) -> i64;
    fn s7_read(p: *const S7) -> i64;
    fn s8_read(p: *const S8) -> i64;
    fn s9_read(p: *const S9) -> i64;
    fn s1_arr_read(p: *const S1, n: i64) -> i64;
    fn s5_fill(p: *mut S5);
    fn s9_fill(p: *mut S9);
    fn s4_fill(p: *mut S4);
}
fn main() {
    println(f"S1 {S1.size_of()}/{lay_size_S1()} {S1.align_of()}/{lay_align_S1()}");
    println(f"S2 {S2.size_of()}/{lay_size_S2()} {S2.align_of()}/{lay_align_S2()}");
    println(f"S3 {S3.size_of()}/{lay_size_S3()} {S3.align_of()}/{lay_align_S3()}");
    println(f"S4 {S4.size_of()}/{lay_size_S4()} {S4.align_of()}/{lay_align_S4()}");
    println(f"S5 {S5.size_of()}/{lay_size_S5()} {S5.align_of()}/{lay_align_S5()}");
    println(f"S6 {S6.size_of()}/{lay_size_S6()} {S6.align_of()}/{lay_align_S6()}");
    println(f"S7 {S7.size_of()}/{lay_size_S7()} {S7.align_of()}/{lay_align_S7()}");
    println(f"S8 {S8.size_of()}/{lay_size_S8()} {S8.align_of()}/{lay_align_S8()}");
    println(f"S9 {S9.size_of()}/{lay_size_S9()} {S9.align_of()}/{lay_align_S9()}");
    let s1 = S1 { a: 1, b: 234, c: 5 };
    println(s1_read(ptr.const(s1)));
    let s2 = S2 { a: 1, b: 2.0, c: 3 };
    println(s2_read(ptr.const(s2)));
    let s3 = S3 { x: 7, inner: S1 { a: 1, b: 234, c: 5 }, y: 6 };
    println(s3_read(ptr.const(s3)));
    let s4 = S4 { a: 4, arr: [11, 22, 33], z: 5 };
    println(s4_read(ptr.const(s4)));
    let s5 = S5 { f: true, ch: 0x1F600, h: -12, g: 2.5, i: -3 };
    println(s5_read(ptr.const(s5)));
    let s6 = S6 { a: 9, b: 4 };
    println(s6_read(ptr.const(s6)));
    let s7 = S7 { e8: E8.B, e32: E32.Y, e16: E16.Q, p: ptr.null() };
    println(s7_read(ptr.const(s7)));
    let s8 = S8 { fd: Fd(31), k: 2 };
    println(s8_read(ptr.const(s8)));
    let s9 = S9 { a: 9, s: S2 { a: 1, b: 0.5, c: 2 }, t: [300, 7] };
    println(s9_read(ptr.const(s9)));
    let arr: Array[S1, 3] = [S1 { a: 0, b: 0, c: 11 }, S1 { a: 0, b: 0, c: 22 }, S1 { a: 0, b: 0, c: 33 }];
    println(s1_arr_read(ptr.const(arr[0]), 3));
    let mut f5 = S5 { f: false, ch: 0, h: 0, g: 0.0, i: 0 };
    s5_fill(ptr.mut(f5));
    println(f"{f5.f} {f5.ch} {f5.h} {f5.g} {f5.i}");
    let mut f9 = S9 { a: 0, s: S2 { a: 0, b: 0.0, c: 0 }, t: [0, 0] };
    s9_fill(ptr.mut(f9));
    println(f"{f9.a} {f9.s.a} {f9.s.b} {f9.s.c} {f9.t[0]} {f9.t[1]}");
    let mut f4 = S4 { a: 0, arr: [0, 0, 0], z: 0 };
    s4_fill(ptr.mut(f4));
    println(f"{f4.a} {f4.arr[0]} {f4.arr[1]} {f4.arr[2]} {f4.z}");
}
"##;

const ENUM_KARA: &str = r##"#[repr(C)] enum Color { Red = -1, Green = 40, Blue = 41 }
#[repr(u8)] enum Small { A = 3, B = 100 }
#[repr(C)] struct Tagged { kind: u8, c: Color, n: u16 }
/// # Safety
/// liblay.so, built from lay.c.
unsafe extern "C" {
    pure fn next_color(c: Color) -> Color;
    pure fn color_code(c: Color) -> i32;
    pure fn tagged_bump(t: Tagged) -> Tagged;
    fn tagged_fill(t: *mut Tagged);
    pure fn small_enum_twice(e: Small) -> u8;
}
fn name(c: Color) -> String {
    match c { Color.Red => "red".to_string(), Color.Green => "green".to_string(), Color.Blue => "blue".to_string() }
}
fn main() {
    println(f"{Color.size_of()} {Tagged.size_of()} {Tagged.align_of()}");
    println(name(next_color(Color.Red)));
    println(name(next_color(Color.Green)));
    println(name(next_color(Color.Blue)));
    println(color_code(Color.Red));
    println(color_code(Color.Blue));
    let t = tagged_bump(Tagged { kind: 1, c: Color.Green, n: 9 });
    println(f"{t.kind} {name(t.c)} {t.n}");
    let mut f = Tagged { kind: 0, c: Color.Red, n: 0 };
    tagged_fill(ptr.mut(f));
    println(f"{f.kind} {name(f.c)} {f.n} {f.c.discriminant()}");
    println(small_enum_twice(Small.B));
    let cs: Vec[Color] = [Color.Blue, Color.Red];
    println(f"{cs[0].discriminant()} {Color.values().len()}");
}
"##;

#[test]
fn s15_repr_c_structs_are_laid_out_as_the_c_compiler_lays_them_out() {
    let Some(dir) = project("layout", LAY_KARA) else {
        return;
    };
    runs_as(dir.path(), "S1 6/6 2/2\nS2 24/24 8/8\nS3 24/24 8/8\nS4 20/20 4/4\nS5 20/20 4/4\nS6 16/16 8/8\nS7 24/24 8/8\nS8 8/8 4/4\nS9 40/40 8/8\n12345\n1023\n7123456\n4112535\n4361253425\n94\n742090\n312\n900103707\n112233\ntrue 128512 -12 2.5 -3\n9 1 0.5 2 300 7\n4 11 22 33 5\n");
}

#[test]
fn s15_repr_enums_cross_to_c_by_value_and_inside_structs() {
    let Some(dir) = project("enums", ENUM_KARA) else {
        return;
    };
    runs_as(
        dir.path(),
        "4 12 4\ngreen\nblue\nred\n-2\n82\n1 blue 10\n3 blue 77 41\n200\n41 3\n",
    );
}

#[test]
fn s15_a_safety_comment_needs_a_word_after_the_colon() {
    let dir = Scratch::new("safety-word");
    let run = |src: &str| -> String {
        std::fs::write(dir.path().join("main.kara"), src).unwrap();
        let out = karac(dir.path(), &["check", "main.kara"]);
        String::from_utf8_lossy(&out.stderr).to_string()
    };
    let block = "    let x: i64 = 3;\n    let p: *const i64 = ptr.const(x);\n";
    let bare = format!(
        "fn main() {{\n{block}    // Safety:\n    let v = unsafe {{ *p }};\n    println(v);\n}}\n"
    );
    assert!(run(&bare).contains("undocumented_unsafe"), "{bare}");
    for ok in [
        "    // Safety: p points at x.\n",
        "    // SAFETY: p points at x.\n",
        "    // Safety:\n    // p points at x, which outlives the block.\n",
    ] {
        let src =
            format!("fn main() {{\n{block}{ok}    let v = unsafe {{ *p }};\n    println(v);\n}}\n");
        let err = run(&src);
        assert!(!err.contains("undocumented_unsafe"), "{src}\n{err}");
    }
}
