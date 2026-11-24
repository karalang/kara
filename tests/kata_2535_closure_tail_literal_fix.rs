//! Kata 2535's Rust template: `nums.iter().map(|&x| { let mut x = x;
//! let mut s = 0; while x > 0 { s += x % 10; x /= 10; } s }).sum()` into
//! an `i32` binding. The `+=` does not fix the literal's type (design.md
//! §8 Literals), so `s` is an `i64`, and the fix declares it `s: i32` as
//! it does for a function that returns such a binding. A closure body
//! with statements before its tail had no fix (B-2026-11-24-4).

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-k2535-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    f
}

fn karac(args: &[&str], f: &PathBuf) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(f)
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

const DIGITS: &str = "fn main() {
    let nums: Vec[i32] = [15i32, 6, 1234].to_vec();
    let digit_sum: i32 = nums
        .iter()
        .map(|x| {
            let mut x = x;
            let mut s = 0;
            while x > 0 {
                s += x % 10;
                x /= 10;
            }
            s
        })
        .sum();
    println(digit_sum);
}
";

#[test]
fn a_closure_tail_literal_summed_into_an_i32_gets_its_annotation() {
    let f = program("digits", DIGITS);
    let (ok, out) = karac(&["check"], &f);
    assert!(!ok, "check accepted the i64 sum: {out}");
    assert!(out.contains("declare it `s: i32`"), "{out}");
    let (ok, out) = karac(&["fix"], &f);
    assert!(ok, "fix: {out}");
    let fixed = std::fs::read_to_string(&f).unwrap();
    assert!(fixed.contains("let mut s: i32 = 0;"), "{fixed}");
    let (ok, out) = karac(&["check"], &f);
    assert!(ok, "check after fix: {out}");
    let (ok, out) = karac(&["run", "--interp"], &f);
    assert!(ok, "run: {out}");
    assert_eq!(out, "22\n");
}

#[test]
fn a_closure_tail_that_is_not_its_own_binding_gets_no_annotation() {
    let f = program(
        "outer",
        "fn main() {
    let nums: Vec[i32] = [15i32, 6].to_vec();
    let mut s = 0;
    let total: i32 = nums.iter().map(|x| { s += 1; s }).sum();
    println(total);
}
",
    );
    let (_, out) = karac(&["check"], &f);
    assert!(!out.contains("declare it `s: i32`"), "{out}");
}
