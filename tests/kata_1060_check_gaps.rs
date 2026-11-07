//! Rust spellings kata 1060's arms met, each carried by `karac fix` to Kāra.
//!
//! - `let (mut lo, mut hi) = (0usize, n - 1);` over an `i64` `n`: once the
//!   `usize` mixes with `i64`, the fix deletes the `usize` suffix, as it
//!   rewrites a written `usize` type to `i64` (B-2026-11-06-3).

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
    std::env::temp_dir().join(format!("karac-kata-1060-{name}-{}", std::process::id()))
}

#[test]
fn kata_1060_usize_suffix_that_mixes_is_dropped() {
    let src = "fn main() {\n    let v = [3, 5, 9];\n    let n = v.len();\n    let (mut lo, mut hi) = (0usize, n - 1);\n    while lo < hi {\n        lo += 1;\n    }\n    println(f\"{lo} {hi}\");\n}";
    let (first, text, clean, out) = mend(&scratch("suffix"), src);
    assert!(first.contains("drop the suffix"), "{first}");
    assert!(text.contains("(0, n - 1)"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "2 2\n");
}

#[test]
fn kata_1060_usize_suffix_alone_draws_nothing() {
    let src = "fn main() {\n    let a = 7usize;\n    let b: usize = 2;\n    println(a + b);\n}";
    let (first, text, clean, out) = mend(&scratch("alone"), src);
    assert!(!first.contains("drop the suffix"), "{first}");
    assert!(text.contains("7usize"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "9\n");
}
