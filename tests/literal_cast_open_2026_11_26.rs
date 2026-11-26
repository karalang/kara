//! B-2026-11-26-1: an `as` cast reads its operand's number without fixing
//! its type (design.md §8 Literals), so `let mid = 5; mid as f64` leaves
//! `mid` open for a later use (a return, an argument) to fix, wherever the
//! cast sits relative to that use.

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-literal-cast-open-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    file
}

fn karac(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

// Kata 1283's float arm: `mid` is cast before anything fixes it, then `lo`
// is returned through an `i32` slot. `big` is fixed to `u32` after its cast,
// so the cast widens an unsigned 4000000000 and must not sign-extend it.
const CASTS: &str = "fn f() -> i32 {
    let mid = 5;
    let x = mid as f64;
    println(f\"{x}\");
    mid
}

fn take(x: i32) -> i32 {
    x * 2
}

fn take_u32(x: u32) -> u32 {
    x
}

fn take_f32(x: f32) -> f32 {
    x
}

fn main() {
    println(f());
    let m = 7;
    let y = m as i64;
    println(f\"{y} {take(m)}\");
    let big = 4000000000;
    let w = big as i64;
    println(f\"{w} {take_u32(big)}\");
    let g = 2.5;
    let n = g as i32;
    println(f\"{n} {take_f32(g)}\");
    let d = 3;
    println(d as f64);
    let mut lo = 1;
    let mut hi = 1000000;
    while lo < hi {
        let mid = (lo + hi) / 2;
        if (17.0 / mid as f64).ceil() <= 3.0 {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    println(take(lo));
}
";

// Worked by hand: ceil(17 / mid) <= 3 first holds at mid = 6, so the search
// ends at lo = 6 and `take` doubles it.
const CASTS_EXPECTED: &str = "5.0
5
7 14
4000000000 4000000000
2 2.5
3.0
12
";

#[test]
fn a_cast_leaves_its_literal_binding_open_interpreted() {
    let file = program("casts-interp", CASTS);
    let f = file.to_str().unwrap();
    let (ok, out) = karac(&["check", f]);
    assert!(ok, "{out}");
    // `m` takes `i32` from `take`, so its `as i64` is not redundant.
    assert!(!out.contains("redundant_cast"), "{out}");
    let (ok, out) = karac(&["__mir-run", f]);
    assert!(ok, "{out}");
    assert_eq!(out, CASTS_EXPECTED);
}

#[cfg(feature = "llvm")]
#[test]
fn a_cast_leaves_its_literal_binding_open_compiled() {
    let file = program("casts-run", CASTS);
    let (ok, out) = karac(&["run", file.to_str().unwrap()]);
    assert!(ok, "{out}");
    assert_eq!(out, CASTS_EXPECTED);
}

#[test]
fn a_literal_fixed_after_its_cast_is_still_range_checked() {
    let file = program(
        "range",
        "fn take_u8(x: u8) -> u8 {\n    x\n}\n\nfn main() {\n    let k = 300;\n    let b = k as i64;\n    println(f\"{b} {take_u8(k)}\");\n}\n",
    );
    let (ok, out) = karac(&["check", file.to_str().unwrap()]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("integer literal 300 out of range for 'u8'"),
        "{out}"
    );
}

#[test]
fn a_cast_to_a_non_number_still_settles_the_literal() {
    let file = program(
        "char",
        "fn main() {\n    let c = 65;\n    println(c as char);\n}\n",
    );
    let (ok, out) = karac(&["check", file.to_str().unwrap()]);
    assert!(!ok, "{out}");
    assert!(out.contains("cannot cast `i64` to `char`"), "{out}");
}

#[test]
fn a_cast_of_a_literal_nothing_else_fixes_is_still_redundant() {
    let file = program(
        "redundant",
        "fn main() {\n    let m = 7;\n    let y = m as i64;\n    println(y);\n}\n",
    );
    let (ok, out) = karac(&["check", file.to_str().unwrap()]);
    assert!(ok, "{out}");
    assert!(
        out.contains("redundant cast: the value is already 'i64'"),
        "{out}"
    );
}
