//! Kata 2024's Rust arm: LeetCode's template keeps its window's left end
//! as `let (mut l, mut cnt, mut res) = (0usize, 0, 0)`, and the end meets
//! the `i64` of `0..s.len()` only in arithmetic, `r - l + 1`.
//!
//! - The `usize` suffix had its fix to `i64` only when an assignment or a
//!   comparison mixed the two; an arithmetic mix with no `as usize` to drop
//!   was refused with no fix at all.

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
    std::env::temp_dir().join(format!("karac-kata-2024-{name}-{}", std::process::id()))
}

#[test]
fn kata_2024_usize_suffix_mixed_only_in_arithmetic_gets_its_fix() {
    let src = "fn longest(v: Vec[i64]) -> i64 {\n    let mut (l, best) = (0usize, 0);\n    for r in 0..v.len() {\n        if v[l] < 0 {\n            l += 1;\n        }\n        best = best.max(r - l + 1);\n    }\n    best\n}\n\nfn main() {\n    println(longest([-1, 2, 3]));\n}";
    let (first, text, clean, out) = mend(&scratch("usize_suffix"), src);
    assert!(first.contains("drop the suffix"), "{first}");
    assert!(text.contains("let mut (l, best) = (0, 0);"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "2\n");
}
