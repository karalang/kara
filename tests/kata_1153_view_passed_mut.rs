//! Found by kata 1153's constructive arm (B-2026-11-14-29).
//!
//! `let mut s = str1.bytes();` binds a read-only `Slice[u8]`, a view of
//! the string's bytes; `let mut` does not make it writable. Passed as
//! `convert(mut s, ..)` to a `mut ref Vec[u8]` parameter it was refused
//! with only "expected 'mut ref Vec[u8]', found 'Slice[u8]'" and no fix.
//! `.to_vec()` at the argument would hand the callee a temporary and lose
//! its writes, so the fix copies the elements where `s` is bound.

use std::process::{Command, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("karac-k1153-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path
}

const CONVERT: &str = "fn convert(s: mut ref Vec[u8], x: u8, y: u8) {\n    for i in 0..s.len() {\n        if s[i] == x {\n            s[i] = y;\n        }\n    }\n}\n\n";

#[test]
fn a_view_passed_as_mut_is_copied_where_it_is_bound() {
    let src = format!(
        "{CONVERT}fn main() {{\n    let word = \"abcab\";\n    let mut s = word.bytes();\n    convert(mut s, b'a', b'z');\n    convert(mut s, b'b', b'y');\n    println(f\"{{String.from_utf8(s).unwrap()}}\");\n}}\n"
    );
    let path = write("fix", &src);
    let check = karac(&["check"], &path);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    assert!(
        err.contains("`s` is a read-only `Slice`")
            && err.contains("`.to_vec()` where `s` is bound"),
        "{err}"
    );
    let fix = karac(&["fix"], &path);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(&path).unwrap();
    let check = karac(&["check"], &path);
    let run = karac(&["__mir-run"], &path);
    let _ = std::fs::remove_file(&path);
    // One edit for both calls, on the binding, not on either argument.
    assert!(
        fixed.contains("let mut s = word.bytes().to_vec();")
            && fixed.contains("convert(mut s, b'a', b'z');")
            && fixed.contains("convert(mut s, b'b', b'y');"),
        "{fixed}"
    );
    assert!(
        !String::from_utf8_lossy(&check.stderr).contains("error"),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "zyczy");
}

#[test]
fn a_view_passed_as_mut_without_a_binding_keeps_its_message() {
    // No `let` to copy at: the refusal stands and no edit is offered.
    let src = format!(
        "{CONVERT}fn main() {{\n    let word = \"abc\";\n    convert(mut word.bytes(), b'a', b'z');\n}}\n"
    );
    let path = write("nolet", &src);
    let check = karac(&["check", "--output=json"], &path);
    let _ = std::fs::remove_file(&path);
    let out = String::from_utf8_lossy(&check.stdout).into_owned();
    assert!(out.contains("Slice[u8]"), "{out}");
    assert!(!out.contains("where `"), "{out}");
}
