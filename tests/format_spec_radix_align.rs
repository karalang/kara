//! design.md §3 Format specifiers (review-thread ruling 2026-11-14), on the
//! MIR interpreter and the compiled backend:
//!
//! - Under `x`, `X`, `o` and `b` a negative value prints its two's
//!   complement at its OWN type's width, as Rust does: `-1i8` is `ff`,
//!   `-2i32` is `fffffffe`. Both backends printed the 64-bit pattern for a
//!   narrow value (B-2026-11-14-41). `+` with one of those types is refused at the
//!   hole; `+` with `d` stays accepted.
//! - With a width and no align a number right-aligns and every other value
//!   (`String`, `Str`, `char`, `bool`, a user type's `Display`) left-aligns,
//!   as in Rust and Python. Every value right-aligned (B-2026-11-14-42).

use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn karac(mode: &str, src: &str) -> Output {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-fs-radix-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(mode)
        .arg(&file)
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

const RADIX: &str = r#"struct Reg {
    lo: i8,
}

fn hex(r: ref Reg) -> String {
    f"{r.lo:x}"
}

fn main() {
    let m: i8 = -1;
    let k: i32 = -2;
    println(f"{m:x}|{k:x}|{m:b}");
    let s: i16 = -1;
    let n: i64 = -255;
    let w: i128 = -1;
    println(f"{s:o}|{s:X}|{n:x}|{w:x}");
    let lo: i8 = -128;
    let hi: i8 = 127;
    let u: u8 = 255;
    println(f"{lo:x}|{hi:x}|{u:x}|{u:b}|{lo:#>6x}|{m:04x}");
    println(hex(Reg { lo: -16 }));
    println(f"{m:d}|{k:+d}|{k:+}|{u:+}");
}
"#;

const RADIX_WANT: &str = "ff|fffffffe|11111111\n\
                          177777|FFFF|ffffffffffffff01|ffffffffffffffffffffffffffffffff\n\
                          80|7f|ff|11111111|####80|00ff\n\
                          f0\n\
                          -1|-2|-2|+255\n";

const ALIGN: &str = r#"struct Point {
    x: i64,
    y: i64,
}

impl Display for Point {
    fn to_string(ref self) -> String {
        f"({self.x},{self.y})"
    }
}

fn main() {
    let name = "ab".to_string();
    let view: Str = "cd";
    let c = 'z';
    let t = true;
    let p = Point { x: 1, y: 2 };
    println(f"[{name:5}][{view:4}][{c:3}][{t:6}][{p:7}]");
    let v = 42;
    let b: u8 = 7;
    let f = 1.5;
    let g: f32 = 2.5;
    let big: i128 = 9;
    println(f"[{v:5}][{b:3}][{f:6}][{g:5}][{big:3}][{v:x}][{v:5x}]");
    println(f"[{name:>5}][{c:^3}][{v:<5}][{t:*^8}][{p:>7}]");
}
"#;

const ALIGN_WANT: &str = "[ab   ][cd  ][z  ][true  ][(1,2)  ]\n\
                          [   42][  7][   1.5][  2.5][  9][2a][   2a]\n\
                          [   ab][ z ][42   ][**true**][  (1,2)]\n";

fn check(mode: &str, src: &str, want: &str) {
    let out = karac(mode, src);
    assert!(
        out.status.success(),
        "{mode}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{mode}");
}

#[test]
fn interp_negative_radix_at_own_width() {
    check("__mir-run", RADIX, RADIX_WANT);
}

#[test]
fn interp_default_alignment_by_kind() {
    check("__mir-run", ALIGN, ALIGN_WANT);
}

#[test]
fn sign_flag_with_a_radix_type_is_refused_at_the_hole() {
    for spec in ["+x", "+X", "+o", "+b", "+08x"] {
        let src = format!("fn main() {{ let w: i8 = -1; println(f\"{{w:{spec}}}\"); }}\n");
        let out = karac("check", &src);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{spec}: accepted");
        assert!(
            err.contains("the sign flag (`+`) is not valid with a radix type"),
            "{spec}: {err}"
        );
    }
    let ok = karac(
        "check",
        "fn main() { let w: i8 = -1; println(f\"{w:+d}|{w:+}\"); }\n",
    );
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_negative_radix_at_own_width() {
    check("__mir-llvm-run", RADIX, RADIX_WANT);
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_default_alignment_by_kind() {
    check("__mir-llvm-run", ALIGN, ALIGN_WANT);
}
