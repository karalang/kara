//! Rust spellings kata 2003's arms met, each carried by `karac fix` to Kāra.
//!
//! - `(nums[u] as usize) < seen.len()` over an `i32` `nums`: the fix for a
//!   `usize` cast that meets `i64` deleted the cast only when its operand was
//!   already `i64`, so over an `i32` the comparison had no fix. The cast's
//!   target now becomes `i64`, which every narrower integer widens to.
//! - `let n = n as usize;` over an `i32` parameter (kata 2005's Rust arm):
//!   the same fix, through the `let` path, whose message said to drop the
//!   cast while the edit wrote `as i64`. The message now names the edit.

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
    std::env::temp_dir().join(format!("karac-kata-2003-{name}-{}", std::process::id()))
}

#[test]
fn kata_2003_usize_cast_of_an_i32_compared_with_len_becomes_i64() {
    let src = "fn main() {\n    let nums: Vec[i32] = [3, 9];\n    let seen = [false, false, false, false];\n    let mut hits = 0;\n    for i in 0..2 {\n        if (nums[i] as usize) < seen.len() {\n            hits += 1;\n        }\n    }\n    println(hits);\n}";
    let (first, text, clean, out) = mend(&scratch("cmp"), src);
    assert!(first.contains("so cast to `i64`"), "{first}");
    assert!(!text.contains("as usize"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "1\n");
}

#[test]
fn kata_2003_usize_cast_of_an_i64_is_still_dropped() {
    let src = "fn main() {\n    let v = [1, 2, 3];\n    let k = 2;\n    if k as usize < v.len() {\n        println(\"in\");\n    }\n}";
    let (first, text, clean, out) = mend(&scratch("drop"), src);
    assert!(first.contains("drop the `as usize`"), "{first}");
    assert!(text.contains("if k < v.len()"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "in\n");
}

#[test]
fn kata_2005_usize_cast_let_of_an_i32_says_cast_to_i64() {
    let src = "fn last(n: i32) -> i64 {\n    let n = n as usize;\n    let g = Vec.filled(n + 1, 7);\n    g[n]\n}\n\nfn main() {\n    println(last(3));\n}";
    let (first, text, clean, out) = mend(&scratch("let"), src);
    assert!(first.contains("so cast to `i64`"), "{first}");
    assert!(!first.contains("drop the `as usize`"), "{first}");
    assert!(text.contains("let n = n as i64;"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "7\n");
}
