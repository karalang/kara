//! Kata 1106's reduce arm rebuilds its string with Rust's `format!("{}{}{}",
//! &s[..open - 1], if value { "t" } else { "f" }, &s[close + 1..])`. The
//! rewrite to an f-string refused any argument containing `f"`, meant to
//! keep an f-string out of a hole, and so refused the plain literal `"f"`
//! too: the macro was left with advice alone. An `f"` opens an f-string
//! only when the `f` is not the end of a longer word or literal, so the
//! check now looks at the character before the `f` (B-2026-11-10-50).

use std::process::{Command, Stdio};

/// `src` after `karac fix` until nothing changes, and its `check` stderr.
fn fixed(tag: &str, src: &str) -> (String, String) {
    let path =
        std::env::temp_dir().join(format!("karac-kata1106-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    for _ in 0..6 {
        let before = std::fs::read_to_string(&path).unwrap();
        let _ = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(["fix", path.to_str().unwrap()])
            .stdin(Stdio::null())
            .output()
            .expect("karac");
        if std::fs::read_to_string(&path).unwrap() == before {
            break;
        }
    }
    let check = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let out = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    (out, String::from_utf8_lossy(&check.stderr).into_owned())
}

#[test]
fn kata_1106_format_with_an_f_literal_argument_is_rewritten() {
    let src = "fn letter(s: String, value: bool) -> String {
    format!(\"{}{}{}\", &s[..1], if value { \"t\" } else { \"f\" }, &s[2..])
}

fn main() {
    println(letter(\"abc\", true));
    println(letter(\"abc\", false));
}
";
    let (text, check) = fixed("f-literal", src);
    assert!(!text.contains("format!"), "{text}");
    assert!(text.contains("if value { \"t\" } else { \"f\" }"), "{text}");
    assert!(
        check.contains("All checks passed") || check.is_empty(),
        "{check}"
    );
    let out = karac::mir::lower::run_source(&text)
        .unwrap_or_else(|e| panic!("{e}\n{text}"))
        .output;
    assert_eq!(out, "atc\nafc\n");
}

#[test]
fn kata_1106_format_with_an_f_string_argument_keeps_the_advice() {
    let src = "fn main() {
    let x = 3;
    let s = format!(\"{}!\", f\"{x}\");
    println(s);
}
";
    let (text, check) = fixed("f-string", src);
    assert!(
        text.contains("format!"),
        "an f-string in a hole stays: {text}"
    );
    assert!(check.contains("format!"), "{check}");
}
