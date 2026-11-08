//! Kata 1074, probing `Entry` chains for the Rust-template arm: Rust's
//! `m.entry(k).and_modify(|v| *v += 1).or_insert(1)` writes an assignment
//! as a closure's body. Assignment is a statement in Kāra (design.md §
//! Assignment) and a closure's body an expression, so the closure takes a
//! block; the parser stopped at the operator with "Expected `)`, found
//! `+=`" and `karac fix` had nothing to apply. The error now names the
//! assignment and the fix writes the braces (B-2026-11-08-68).

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

/// The first check's diagnostics, the text one `karac fix` leaves, whether
/// that text checks clean, and what it prints under the MIR interpreter.
fn one_round(name: &str, src: &str) -> (String, String, bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-1074-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), format!("{src}\n")).unwrap();
    let first = karac(&dir, &["check", "cell.kara"]).2;
    karac(&dir, &["fix", "cell.kara"]);
    let text = std::fs::read_to_string(dir.join("cell.kara")).unwrap();
    let (clean, _, _) = karac(&dir, &["check", "cell.kara"]);
    let (_, out, _) = karac(&dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (first, text, clean, out)
}

#[test]
fn kata_1074_an_assigning_closure_body_gets_its_braces() {
    let src = "fn main() {\n    let mut m: Map[i64, i64] = Map.new();\n    for k in [1, 2, 1] {\n        m.entry(k).and_modify(|v| *v += 1).or_insert(1);\n    }\n    let mut seen: Vec[i64] = Vec.new();\n    [4, 5].iter().for_each(|x| seen.push(*x));\n    let mut last = 0;\n    [7, 8].iter().for_each(|x| last = *x);\n    println(f\"{m.get(1).unwrap_or(0)} {m.get(2).unwrap_or(0)} {seen} {last}\");\n}";
    let (first, text, clean, out) = one_round("assign", src);
    assert!(
        first.contains("`+=` is an assignment, a statement"),
        "{first}"
    );
    assert!(
        first.contains("`=` is an assignment, a statement"),
        "{first}"
    );
    assert!(!first.contains("Expected `)`"), "{first}");
    assert!(clean, "still refused after one fix:\n{text}");
    assert!(text.contains(".and_modify(|v| { *v += 1; })"), "{text}");
    assert!(text.contains(".for_each(|x| { last = *x; })"), "{text}");
    assert!(text.contains(".for_each(|x| seen.push(*x))"), "{text}");
    assert_eq!(out.trim_end(), "2 1 [4, 5] 8", "{text}");
}
