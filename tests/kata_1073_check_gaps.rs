//! Kata 1073's Rust arm (LeetCode's template for adding two negabinary
//! numbers) keeps its carry as `let mut carry = 0;`, copies it into the
//! digit sum (`let mut x = carry;`), adds `i32` digits to that and pushes
//! `x & 1` onto a `Vec[i32]`. Only operators use the literal, so it keeps
//! its default `i64` (design.md §8 Literals) and the push is refused. The
//! refusal offered the binding's annotation (B-2026-11-08-20) only for a
//! literal let named in the refused expression, not for a copy of one, so
//! `karac fix` had nothing to apply. A copy is now traced to its literal.

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
    let dir = std::env::temp_dir().join(format!("karac-kata-1073-{name}-{}", std::process::id()));
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
fn kata_1073_a_copied_literal_binding_gets_its_annotation() {
    let src = "fn low_digits(a: Vec[i32]) -> Vec[i32] {\n    let mut carry = 0;\n    let mut out: Vec[i32] = Vec.new();\n    for d in a.iter() {\n        let mut x = carry;\n        x += d;\n        out.push(x & 1);\n        carry = -(x >> 1);\n    }\n    out\n}\n\nfn main() {\n    let a: Vec[i32] = [1, 1, 1];\n    println(f\"{low_digits(a)}\");\n}";
    let (first, text, clean, out) = one_round("copy", src);
    assert!(first.contains("declare it `carry: i32`"), "{first}");
    assert!(clean, "still refused after one fix:\n{text}");
    assert!(text.contains("    let mut carry: i32 = 0;\n"), "{text}");
    assert!(text.contains("        let mut x = carry;\n"), "{text}");
    assert_eq!(out.trim_end(), "[1, 1, 1]", "{text}");
}

#[test]
fn kata_1073_a_copy_of_a_fixed_binding_is_left_alone() {
    let src = "fn main() {\n    let carry = 0;\n    let x = carry;\n    let small: i32 = 7;\n    let y: i64 = x + 1;\n    println(f\"{y} {small}\");\n}";
    let (first, text, clean, out) = one_round("fixed", src);
    assert!(!first.contains("error"), "{first}");
    assert!(clean, "{text}");
    assert_eq!(out.trim_end(), "1 7", "{text}");
}
