//! Kata 2023's Rust arm: LeetCode's template cuts the target with
//! `target.split_at(i)`, which Kāra's `Str` and `String` did not have.
//!
//! - `split_at(mid)` gives the text before and after byte `mid`, both views,
//!   as `split_once` gives them around a separator; a `mid` past the end
//!   panics, as `s[a..b]` does.

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
    std::env::temp_dir().join(format!("karac-kata-2023-{name}-{}", std::process::id()))
}

#[test]
fn kata_2023_split_at_cuts_a_string_and_a_str() {
    let src = "fn main() {\n    let s: String = \"7777\";\n    let mut cuts = 0;\n    for i in 1..s.len() {\n        let (a, b) = s.split_at(i);\n        if a.len() + b.len() == s.len() and a == s[0..i] {\n            cuts += 1;\n        }\n    }\n    let t: Str = \"hello\";\n    let (x, y) = t.split_at(2);\n    let (p, q) = t.split_at(5);\n    println(f\"{cuts} {x}|{y} {p}|{q.len()}\");\n}";
    let (first, _text, clean, out) = mend(&scratch("split_at"), src);
    assert!(!first.contains("error"), "{first}");
    assert!(clean);
    assert_eq!(out, "3 he|llo hello|0\n");
}

#[test]
fn kata_2023_split_at_past_the_end_panics() {
    let dir = scratch("split_at_past_end");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("cell.kara"),
        "fn main() {\n    let t: Str = \"abc\";\n    let (a, b) = t.split_at(4);\n    println(f\"{a}|{b}\");\n}\n",
    )
    .unwrap();
    let (checked, _, err) = karac(&dir, &["check", "cell.kara"]);
    assert!(checked, "{err}");
    let (ran, out, _) = karac(&dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!ran, "{out}");
    assert!(!out.contains('|'), "{out}");
}
