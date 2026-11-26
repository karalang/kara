//! The fix that joins text pieces a `String` does not gather,
//! `Vec[String].from_iter(..).join("")`, replaces the `collect` call from
//! the `(` of a parenthesized receiver (B-2026-11-26-13).
//!
//! The call's node starts inside the `(` that opens `(0..250).map(..)`,
//! while the replacement renders the receiver with its parentheses, so the
//! edit left a stray `(` and `karac fix`'s parse guard refused it.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

/// `karac fix` then `karac check` on `src`: the fixed text, and whether it
/// checks clean.
fn fix_then_check(src: &str) -> (String, bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-fix-collect-paren-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let _ = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("fix")
        .arg(&f)
        .output()
        .unwrap();
    let fixed = std::fs::read_to_string(&f).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        fixed,
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn runs(src: &str, want: &str) {
    let dir = std::env::temp_dir().join(format!(
        "karac-fix-collect-paren-run-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-run")
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{src}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{src}");
}

fn joined(expr: &str, want_text: &str, want_out: &str) {
    let src = format!(
        "fn main() {{ let n = 3; let v: Vec[i64] = [1, 2]; let half: String = {expr}; println(half); }}\n"
    );
    let (fixed, ok, out) = fix_then_check(&src);
    assert!(fixed.contains(want_text), "{src}\nfixed to:\n{fixed}");
    assert!(ok, "{fixed}\n{out}");
    runs(&fixed, want_out);
}

#[test]
fn a_parenthesized_range_receiver_is_joined_whole() {
    joined(
        "(0..4).map(|i| f\"{i % 10}q\").collect()",
        "let half: String = Vec[String].from_iter((0..4).map(|i| f\"{i % 10}q\")).join(\"\");",
        "0q1q2q3q\n",
    );
    joined(
        "(0..3).rev().map(|i| f\"{i}q\").collect()",
        "Vec[String].from_iter((0..3).rev().map(|i| f\"{i}q\")).join(\"\")",
        "2q1q0q\n",
    );
    joined(
        "(0..n).map(|i| i.to_string()).collect()",
        "Vec[String].from_iter((0..n).map(|i| i.to_string())).join(\"\")",
        "012\n",
    );
    joined(
        "((0..2)).map(|i| f\"{i}q\").collect()",
        "Vec[String].from_iter(",
        "0q1q\n",
    );
}

#[test]
fn an_unparenthesized_receiver_is_joined_as_before() {
    joined(
        "v.iter().map(|i| f\"{i}\").collect()",
        "let half: String = Vec[String].from_iter(v.iter().map(|i| f\"{i}\")).join(\"\");",
        "12\n",
    );
    joined(
        "(v.iter()).map(|i| f\"{i}\").collect()",
        "Vec[String].from_iter(",
        "12\n",
    );
}
