//! A mismatch whose found type is an `Option` or `Result` wrapping the
//! expected one is a forgotten unwrap, and the diagnostic says so
//! (B-2026-11-03-21). The arithmetic mismatch carried the hint since
//! B-2026-07-30-13; a `let` annotation, a call argument, `String.push`
//! and the if/else and match branch joins said only "expected 'char',
//! found 'Option[char]'". Kata 1041 met it on `s.push(t.char_at(i))`.

use std::process::{Command, Stdio};

const HINT: &str = "wraps the 'char'; unwrap it first";

/// `karac check --output=json`: each diagnostic's line and message.
fn check(tag: &str, src: &str) -> Vec<(u64, String)> {
    let path = std::env::temp_dir().join(format!(
        "karac-option-unwrap-hint-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "--output=json"])
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{tag}: {e}"));
    json["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .map(|d| {
            (
                d["line"].as_u64().unwrap_or(0),
                d["message"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

/// One diagnostic, on `line`, carrying `hint`.
fn hinted(tag: &str, src: &str, line: u64, hint: &str) {
    let got = check(tag, src);
    assert_eq!(got.len(), 1, "{tag}: {got:?}");
    assert_eq!(got[0].0, line, "{tag}: {got:?}");
    assert!(got[0].1.contains(hint), "{tag}: {got:?}");
}

#[test]
fn unwrap_hint_on_a_let_annotation() {
    hinted(
        "let",
        "fn main() {\n    let s = \"abc\";\n    let c: char = s.char_at(0);\n    println(f\"{c}\");\n}\n",
        3,
        HINT,
    );
}

#[test]
fn unwrap_hint_on_a_call_argument() {
    hinted(
        "arg",
        "fn take(c: char) -> i64 {\n    1\n}\n\nfn main() {\n    let s = \"abc\";\n    println(f\"{take(s.char_at(1))}\");\n}\n",
        7,
        HINT,
    );
}

#[test]
fn unwrap_hint_on_string_push() {
    hinted(
        "push",
        "fn main() {\n    let mut out = String.new();\n    out.push(\"GLR\".char_at(1));\n    println(out);\n}\n",
        3,
        HINT,
    );
}

#[test]
fn unwrap_hint_on_if_else_branches() {
    hinted(
        "ifelse",
        "fn main() {\n    let k = 3;\n    let c = if k % 2 == 0 { 'G' } else { \"GLR\".char_at(k % 3) };\n    println(f\"{c}\");\n}\n",
        3,
        HINT,
    );
}

#[test]
fn unwrap_hint_on_match_arms() {
    hinted(
        "match",
        "fn main() {\n    let k = 3;\n    let c = match k {\n        0 => \"GLR\".char_at(k),\n        _ => 'G',\n    };\n    println(f\"{c}\");\n}\n",
        3,
        HINT,
    );
}

#[test]
fn unwrap_hint_on_a_result_argument() {
    hinted(
        "result",
        "fn half(n: i64) -> Result[i64, String] {\n    if n % 2 == 0 { Ok(n / 2) } else { Err(\"odd\".to_string()) }\n}\n\nfn main() {\n    let h: i64 = half(4);\n    println(f\"{h}\");\n}\n",
        6,
        "`Result` wraps the 'i64'; unwrap it first",
    );
}

#[test]
fn unwrap_hint_on_an_option_of_a_ref() {
    hinted(
        "first",
        "fn main() {\n    let v: Vec[i64] = [1, 2];\n    let x: i64 = v.first();\n    println(f\"{x}\");\n}\n",
        3,
        "`Option` wraps the 'ref i64'; unwrap it first",
    );
}

#[test]
fn no_unwrap_hint_when_nothing_is_wrapped() {
    let got = check(
        "plain",
        "fn main() {\n    let c: char = \"abc\";\n    println(f\"{c}\");\n}\n",
    );
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(!got[0].1.contains("unwrap"), "{got:?}");
    let got = check(
        "other",
        "fn main() {\n    let s = \"abc\";\n    let c: i64 = s.char_at(0);\n    println(f\"{c}\");\n}\n",
    );
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(!got[0].1.contains("unwrap"), "{got:?}");
}
