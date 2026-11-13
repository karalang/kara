//! Found by a mutation of kata 1151's prefix-sum arm (B-2026-11-13-35).
//!
//! A `scan` whose state starts at an unsuffixed literal holds that literal
//! open, so a `collect` into a `Vec[i32]` can still fix it (B-2026-11-11-61).
//! When the closure's next state is a number computed without the state
//! and its item is the state, `|prev, x| Some((*x, prev))`, the open
//! literal met that number in the state's slot and was refused: "expected
//! 'Option[(i64, _)]', found 'Option[(i64, i64)]'", two types that unify.
//! The number now fixes the literal, as it would the literal itself.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

/// `karac check`'s stderr ("All checks passed." when clean) and what
/// `karac __mir-run` prints.
fn check_run(tag: &str, src: &str) -> (String, String) {
    let path =
        std::env::temp_dir().join(format!("karac-kata-1151-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let check = String::from_utf8_lossy(&karac(&["check"], &path).stderr).into_owned();
    let out = String::from_utf8_lossy(&karac(&["__mir-run"], &path).stdout).into_owned();
    let _ = std::fs::remove_file(&path);
    (check, out)
}

fn scan_program(elem: &str, out: &str, body: &str) -> String {
    format!(
        "fn main() {{\n    let data: Vec[{elem}] = [1, 0, 1, 1];\n    let a: Vec[{out}] = data.iter().scan(0, |s, x| {body}).collect();\n    println(f\"{{a}}\");\n}}\n"
    )
}

#[test]
fn a_scan_state_computed_without_the_state_fixes_its_literal() {
    let cases = [
        ("i64", "i64", "Some((*x, s))", "[0, 1, 0, 1]"),
        ("i64", "i64", "Some((x + 0, s))", "[0, 1, 0, 1]"),
        ("i64", "i64", "Some((1, s))", "[0, 1, 1, 1]"),
        (
            "i64",
            "i64",
            "{ let t = s + 0; Some((*x, t)) }",
            "[0, 1, 0, 1]",
        ),
        ("i32", "i32", "Some((*x, s))", "[0, 1, 0, 1]"),
        (
            "i64",
            "(i64, i64)",
            "Some((*x, (s, *x)))",
            "[(0, 1), (1, 0), (0, 1), (1, 1)]",
        ),
        // The shapes that ran before, unchanged.
        ("i64", "i64", "Some((s + x, s))", "[0, 1, 1, 2]"),
        ("i64", "i64", "Some((s - 1, s))", "[0, -1, -2, -3]"),
        ("i64", "i64", "Some((s + x, s + x))", "[1, 1, 2, 3]"),
    ];
    for (i, (elem, out, body, want)) in cases.iter().enumerate() {
        let (check, got) = check_run(&format!("ok{i}"), &scan_program(elem, out, body));
        assert!(
            !check.contains("error"),
            "{body} over Vec[{elem}]: karac check said\n{check}"
        );
        assert_eq!(got.trim(), *want, "{body} over Vec[{elem}]");
    }
}

#[test]
fn a_scan_state_still_refuses_a_number_its_literal_cannot_take() {
    // A float literal's state cannot become an integer, nor an `i64`
    // state's items a `Vec[i32]`'s.
    let cases = [
        ("i64", "f64", "scan(0.5, |s, x| Some((*x, s)))"),
        ("i64", "i32", "scan(0, |s, x| Some((*x, s)))"),
    ];
    for (i, (elem, out, call)) in cases.iter().enumerate() {
        let src = format!(
            "fn main() {{\n    let data: Vec[{elem}] = [1, 0, 1];\n    let a: Vec[{out}] = data.iter().{call}.collect();\n    println(f\"{{a}}\");\n}}\n"
        );
        let (check, _) = check_run(&format!("bad{i}"), &src);
        assert!(
            check.contains("error"),
            "{call} into Vec[{out}] passed karac check"
        );
    }
}
