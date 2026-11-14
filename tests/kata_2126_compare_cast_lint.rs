//! Kata 2126's Rust template compares an `i64` planet with an `i32`
//! asteroid as `if m < a as i64`. A comparison widens its narrower
//! integer operand itself (design.md § Mixed-width operands), so the cast
//! is one `lossless_cast` should flag, as it already did in `m += a as
//! i64` on the next line; `karac fix` dropped that one and left this.
//! A comparison refuses mixed signedness outright, so there only a cast
//! within one signedness is flagged.

use std::process::{Command, Stdio};

fn check(tag: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-k2126-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "--output=json", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn kata_2126_widening_cast_in_a_comparison_is_flagged() {
    let out = check(
        "flagged",
        "fn main() {
    let a: i32 = 7;
    let m: i64 = 5;
    let w: f32 = 0.5;
    let x: f64 = 1.5;
    println(f\"{m < a as i64} {a as i64 == m} {x >= w as f64}\");
}
",
    );
    assert_eq!(out.matches("\"W0291\"").count(), 3, "{out}");
    assert!(
        out.contains("'i32' widens to 'i64' implicitly here"),
        "{out}"
    );
}

#[test]
fn kata_2126_casts_a_comparison_needs_are_kept() {
    let out = check(
        "kept",
        "fn main() {
    let a: i32 = 7;
    let b: i32 = 9;
    let u: u32 = 3;
    let m: i64 = 5;
    println(f\"{u as i64 < m} {(a as i64) < (b as i64)} {a as i64 > 3} {m < a as i64 * 2}\");
}
",
    );
    assert!(!out.contains("\"W0291\""), "{out}");
}
