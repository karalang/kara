//! Kata 1081, Smallest Subsequence of Distinct Characters: the greedy arm
//! writes Rust's `rest.replace(c, "")` to take every copy of a placed
//! letter out of the rest of the text. `find` and `contains` already met a
//! char where they take a `Str` pattern with a `c.to_string()` fix, but
//! `replace` and `replacen` said only "expects String arguments", with no
//! edit, so `karac fix` stopped there. They now get the same fix, on the
//! pattern and on the replacement alike (B-2026-11-09-25).

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
    let dir = std::env::temp_dir().join(format!("karac-kata-1081-{name}-{}", std::process::id()));
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
fn kata_1081_a_char_handed_to_replace_is_fixed_to_a_str() {
    let src = "fn main() {\n    let s = \"banana\";\n    let c = 'a';\n    println(s.replace(c, \"\"));\n    println(s.replace('n', \"N\"));\n    println(s.replacen(c, \"A\", 2));\n    println(s.replace(\"an\", c));\n}";
    let (first, text, clean, out) = one_round("replace", src);
    assert!(
        first.contains("'replace' takes a `Str` pattern, found 'char'"),
        "{first}"
    );
    assert!(
        first.contains("'replacen' takes a `Str` pattern, found 'char'"),
        "{first}"
    );
    assert!(!first.contains("expects String arguments"), "{first}");
    assert!(clean, "still refused after one fix:\n{text}");
    assert!(text.contains("s.replace(c.to_string(), \"\")"), "{text}");
    assert!(text.contains("s.replace(\"n\", \"N\")"), "{text}");
    assert!(
        text.contains("s.replacen(c.to_string(), \"A\", 2)"),
        "{text}"
    );
    assert!(text.contains("s.replace(\"an\", c.to_string())"), "{text}");
    assert_eq!(out.trim_end(), "bnn\nbaNaNa\nbAnAna\nbaaa", "{text}");
}

#[test]
fn kata_1081_an_integer_handed_to_replace_keeps_its_error() {
    let src = "fn main() {\n    println(\"banana\".replace(1, \"\"));\n}";
    let (first, text, clean, _) = one_round("integer", src);
    assert!(
        first.contains("'replace' expects String arguments, found 'i64'"),
        "{first}"
    );
    assert!(!clean, "{text}");
}
