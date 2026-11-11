//! Kata 1121 compares a run's length times `k` with the array's length,
//! `longest * (k as i64) <= nums.len()`, where `k` is an `i32`. The cast
//! widens implicitly there (`lossless_cast`), and `karac fix` deleted the
//! ` as i64` and left `longest * (k)`, parentheses around a single name.
//! A cast in parentheses of its own now goes with them; a parenthesized
//! operand, `((a - b) as i64)`, keeps its own.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kata_1121_paren_cast_{name}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The text `karac fix` leaves, whether it then checks with no
/// diagnostic, and what it prints under the MIR interpreter.
fn fixed(name: &str, src: &str) -> (String, bool, String) {
    let dir = scratch(name);
    let file = dir.join("m.kara");
    std::fs::write(&file, src).unwrap();
    karac(&dir, &["fix", "m.kara"]);
    let text = std::fs::read_to_string(&file).unwrap();
    let (_, json) = karac(&dir, &["check", "--output=json", "m.kara"]);
    let clean = json.contains("\"diagnostics\":[]") || json.contains("\"diagnostics\": []");
    let (_, out) = karac(&dir, &["__mir-run", "m.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (text, clean, out)
}

#[test]
fn a_lossless_cast_in_its_own_parentheses_goes_with_them() {
    let src = "fn fits(longest: i64, k: i32, n: i64) -> bool {
    longest * (k as i64) <= n
}

fn main() {
    let v: Vec[i32] = [1, 2, 3, 4];
    let k: i32 = 2;
    let m = v.len() / (k as i64);
    println(f\"{fits(2, k, 4)} {fits(3, k, 4)} {m}\");
}
";
    let (text, clean, out) = fixed("own", src);
    assert!(text.contains("longest * k <= n"), "{text}");
    assert!(text.contains("let m = v.len() / k;"), "{text}");
    assert!(!text.contains("(k)"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "true false 2\n");
}

#[test]
fn a_cast_of_a_parenthesized_operand_keeps_the_operand_s_parentheses() {
    let src = "fn main() {
    let a: i32 = 7;
    let b: i32 = 3;
    let x: i64 = 2;
    let y = x * ((a - b) as i64);
    println(f\"{y}\");
}
";
    let (text, clean, out) = fixed("operand", src);
    assert!(
        text.contains("x * ((a - b))") || text.contains("x * (a - b)"),
        "{text}"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "8\n");
}

#[test]
fn a_call_argument_cast_keeps_the_call_s_parentheses() {
    let src = "fn twice(n: i64) -> i64 {
    n * 2
}

fn main() {
    let k: i32 = 5;
    println(f\"{twice(k as i64)}\");
}
";
    let (text, clean, out) = fixed("call", src);
    assert!(text.contains("twice(k)"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "10\n");
}
