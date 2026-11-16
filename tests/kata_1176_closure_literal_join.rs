//! Kata 1176's Rust arm scores each window with a closure, `let score =
//! |s: i32| if s < lower { -1 } else if s > upper { 1 } else { 0 };`, and
//! returns the sum of its results as `i32`. Rust types the closure's
//! literals from that use. Kāra typed them `i64` at the closure, so
//! `points` was an `i64` and the `i32` return was refused as a narrowing,
//! with no fix. A closure whose body is a join of literals is now a literal
//! (design.md §8, "A join of literals is a literal"), left open for a use
//! of its result to fix, as a `let`'s value is. B-2026-11-16-211.

use std::path::Path;
use std::process::{Command, Stdio};

/// Runs `karac` on `cell.kara` in `dir`: its verdict, its output, and the
/// file as it stands after.
fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("cell.kara")
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let fixed = std::fs::read_to_string(dir.join("cell.kara")).unwrap_or_default();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        fixed,
    )
}

fn cell(tag: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-kata-1176-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    dir
}

/// One `karac fix` round over `src`, then a second that must find nothing,
/// then `__mir-run`: the source after the one round, and what it prints.
fn one_round(tag: &str, src: &str) -> (String, String) {
    let dir = cell(tag, src);
    let (_, out, fixed) = karac(&dir, &["fix"]);
    assert!(out.contains("applied"), "{tag}: {out}");
    let (_, out, again) = karac(&dir, &["fix"]);
    assert_eq!(
        again, fixed,
        "{tag}: a second round changed the source: {out}"
    );
    let (ok, out, _) = karac(&dir, &["check"]);
    assert!(ok, "{tag}: {out}\n{fixed}");
    let (ok, out, _) = karac(&dir, &["__mir-run"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{tag}: {out}\n{fixed}");
    (fixed, out)
}

#[test]
fn kata_1176_scoring_closure_takes_the_return_type() {
    let (fixed, out) = one_round(
        "score",
        r#"struct Solution;

impl Solution {
    pub fn points(totals: Vec<i32>, lower: i32, upper: i32) -> i32 {
        let score = |s: i32| if s < lower { -1 } else if s > upper { 1 } else { 0 };
        let mut points = 0;
        for t in totals {
            points += score(t);
        }
        points
    }
}

fn main() {
    println(f"{Solution::points(vec![1, 5, 9, 9], 3, 7)}");
}
"#,
    );
    assert!(
        fixed.contains("let score = |s: i32| if s < lower { -1 }"),
        "{fixed}"
    );
    assert_eq!(out.trim(), "1");
}

/// A join of literals no use fixes is an `i64`; one an `i32` use fixes is
/// range-checked against `i32`.
#[test]
fn kata_1176_open_closure_join_defaults_or_is_range_checked() {
    let dir = cell(
        "default",
        "fn main() {\n    let big = |c: bool| if c { 3000000000 } else { 0 };\n    let g = |c: bool| match c { true => 7, false => 8 };\n    let y: i32 = g(false);\n    println(f\"{big(true)} {y}\");\n}\n",
    );
    let (ok, out, _) = karac(&dir, &["__mir-run"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{out}");
    assert_eq!(out.trim(), "3000000000 8");
    let dir = cell(
        "range",
        "fn f(c: bool) -> i32 {\n    let big = |c: bool| if c { 3000000000 } else { 0 };\n    big(c)\n}\n\nfn main() {\n    println(f\"{f(true)}\");\n}\n",
    );
    let (ok, out, _) = karac(&dir, &["check"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!ok, "{out}");
    assert!(
        out.contains("integer literal 3000000000 out of range for 'i32'"),
        "{out}"
    );
}
