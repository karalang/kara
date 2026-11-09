//! Kata 2035's Rust arm keeps `let mut a = 0; a += nums[i];` over `i32`s,
//! so `a` stays `i64` (design.md §8: an operator does not fix a literal's
//! type) and meets an `i32` receiver at `best.min(a + nums[j])`. The fix,
//! `a: i32` at the binding, was offered only when the defaulted binding was
//! the receiver (`a.min(x)`, B-2026-11-08-53); an argument that reads it,
//! bare, in arithmetic or under a numeric method like `.abs()`, now gets it
//! too.

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Every check's diagnostics, the text `karac fix` leaves, whether that
/// text checks clean, and what it prints under the MIR interpreter.
fn mend(dir: &Path, src: &str) -> (String, String, bool, String) {
    std::fs::create_dir_all(dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(&file, format!("{src}\n")).unwrap();
    let mut first = String::new();
    for _ in 0..4 {
        first += &karac(dir, &["check", "cell.kara"]).2;
        let before = std::fs::read_to_string(&file).unwrap();
        karac(dir, &["fix", "cell.kara"]);
        if std::fs::read_to_string(&file).unwrap() == before {
            break;
        }
    }
    let text = std::fs::read_to_string(&file).unwrap();
    let (clean, _, _) = karac(dir, &["check", "cell.kara"]);
    let (_, out, _) = karac(dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(dir);
    (first, text, clean, out)
}

fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("karac-kata-2035-{name}-{}", std::process::id()))
}

#[test]
fn kata_2035_min_argument_reading_a_defaulted_literal_gets_its_annotation() {
    let src = "fn f(nums: Vec[i32]) -> i32 {\n    let mut a = 0;\n    a += nums[0];\n    let mut best = i32.MAX;\n    best = best.min(a + nums[1]);\n    best = best.max((10 - 2 * a).abs());\n    best\n}\n\nfn main() {\n    println(f([1i32, 2i32].to_vec()));\n}";
    let (first, text, clean, out) = mend(&scratch("min-arg"), src);
    assert!(first.contains("declare it `a: i32`"), "{first}");
    assert!(text.contains("let mut a: i32 = 0;"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "8\n");
}

#[test]
fn kata_2035_receiver_side_annotation_still_wins() {
    let src = "fn f(nums: Vec[i32]) -> i32 {\n    let mut a = 0;\n    a += nums[0];\n    a.min(nums[1])\n}\n\nfn main() {\n    println(f([4i32, 2i32].to_vec()));\n}";
    let (first, text, clean, out) = mend(&scratch("min-recv"), src);
    assert!(first.contains("declare it `a: i32`"), "{first}");
    assert!(text.contains("let mut a: i32 = 0;"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "2\n");
}
