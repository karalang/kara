//! Kata 1071's Rust arm compares two `format!` calls in an `if` head:
//! `if format!("{}{}", a, b) != format!("{}{}", b, a) {`. The macro was
//! recognised only where `expect` met its `!`, after the condition had
//! ended at the first call's name, so the second call was never reported
//! and the `!=` desynchronised the rest of the function: "expected `;`",
//! "Expected expression, found `}`", top-level statements, and a `karac
//! fix` that put a `;` after the function's tail. A macro call is now read
//! as an operand where its name is parsed, so every call is reported and
//! fixed in one round.

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
    let dir = std::env::temp_dir().join(format!("karac-kata-1071-{name}-{}", std::process::id()));
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
fn kata_1071_two_format_calls_in_a_condition_fix_in_one_round() {
    let src = "fn joins(a: String, b: String) -> bool {\n    if format!(\"{}{}\", a, b) != format!(\"{}{}\", b, a) {\n        return false;\n    }\n    let n = a.len();\n    n > 0\n}\n\nfn main() {\n    println(f\"{joins(\"ab\", \"abab\")} {joins(\"a\", \"b\")}\");\n}";
    let (first, text, clean, out) = one_round("cond", src);
    assert_eq!(
        first.matches("`format!(...)` is Rust syntax").count(),
        2,
        "{first}"
    );
    assert!(
        !first.contains("expected `;`") && !first.contains("Expected expression"),
        "{first}"
    );
    assert!(clean, "still refused after one fix:\n{text}");
    assert!(
        text.contains("if f\"{a}{b}\" != f\"{b}{a}\" {") && text.contains("    n > 0\n"),
        "{text}"
    );
    assert_eq!(out.trim_end(), "true false", "{text}");
}

#[test]
fn kata_1071_macro_calls_as_operands_and_statements() {
    let src = "fn main() {\n    let a = 3;\n    let same = format!(\"{}\", a) == format!(\"{}\", 3);\n    let n = format!(\"{}{}\", a, a).len() + 1;\n    println!(\"{} {}\", same, n);\n}";
    let (first, text, clean, out) = one_round("operands", src);
    assert_eq!(first.matches("is Rust syntax").count(), 4, "{first}");
    assert!(clean, "still refused after one fix:\n{text}");
    assert!(
        text.contains("let same = f\"{a}\" == f\"{3}\";")
            && text.contains("let n = f\"{a}{a}\".len() + 1;")
            && text.contains("println(f\"{same} {n}\");"),
        "{text}"
    );
    assert_eq!(out.trim_end(), "true 3", "{text}");
}
