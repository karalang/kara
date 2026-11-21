//! B-2026-11-18-92: a narrowing at a tuple position read in place
//! (`r + dirs[d].0`, `pair.0 * c`) or through a destructuring `let`
//! (`let (s, t) = signs[k]`, `let (s, t) = pair`) got only the bare `as
//! i32` advice, where `for (s, t) in signs` and `let (s, t) = (1, -1)`
//! were offered the suffix on the position's literal. Each is now offered
//! that suffix, which `karac fix` applies. Kata 2326's Rust template and
//! kata 1330's signs arm.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("karac-tuple-pos-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

fn check_text(tag: &str, src: &str) -> String {
    let (dir, path) = fixture(tag, src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn fix_and_run(tag: &str, src: &str) -> (String, bool, String) {
    let (dir, path) = fixture(tag, src);
    for _ in 0..4 {
        karac().arg("fix").arg(&path).output().unwrap();
    }
    let fixed = std::fs::read_to_string(&path).unwrap();
    let check = karac().arg("check").arg(&path).output().unwrap();
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        fixed,
        check.status.success(),
        String::from_utf8_lossy(&run.stdout).into_owned(),
    )
}

/// The row's own program: `let nr = r + dirs[d].0; r = nr;` with `r` an
/// `i32`, the narrowing refused at `nr`, which is an `i64` only because
/// position 0's first literal took the default; `c += dirs[d].1` types
/// position 1 by itself. Six steps around the four directions end at row
/// 1, column 1.
#[test]
fn an_indexed_tuple_position_is_offered_its_literal_suffix() {
    let src = "fn walk(steps: i64) -> i32 {
    let dirs = [(0, 1), (1, 0), (0, -1), (-1, 0)];
    let mut r: i32 = 0;
    let mut c: i32 = 0;
    for k in 0..steps {
        let d = k % 4;
        let nr = r + dirs[d].0;
        r = nr;
        c += dirs[d].1;
    }
    r * 10 + c
}

fn main() {
    println(walk(6).to_string());
}
";
    let diag = check_text("dirs-diag", src);
    assert!(
        diag.contains("position 0 of the tuples in `dirs` is an 'i64'"),
        "{diag}"
    );
    let (fixed, clean, out) = fix_and_run("dirs", src);
    assert!(
        fixed.contains("let dirs = [(0i32, 1), (1, 0), (0, -1), (-1, 0)];"),
        "{fixed}"
    );
    assert!(clean, "{fixed}");
    assert_eq!(out, "11\n");
}

/// `let (s, t) = signs[k]`: as `for (s, t) in signs` was.
#[test]
fn a_tuple_let_of_an_indexed_tuple_is_offered_its_literal_suffix() {
    let src = "fn f(c: i32) -> i32 {
    let signs = [(1, 1), (1, -1)];
    let mut best: i32 = 0;
    for k in 0..2 {
        let (s, t) = signs[k];
        best = best.max(s * c + t * c);
    }
    best
}

fn main() {
    println(f(3).to_string());
}
";
    let (fixed, clean, out) = fix_and_run("signs", src);
    assert!(
        fixed.contains("let signs = [(1i32, 1i32), (1, -1)];"),
        "{fixed}"
    );
    assert!(clean, "{fixed}");
    assert_eq!(out, "6\n");
}

/// `let (s, t) = pair` and `pair.0` of `let pair = (1, -1)`.
#[test]
fn a_tuple_binding_read_whole_or_by_position_is_offered_its_literal_suffix() {
    let whole = "fn f(c: i32) -> i32 {
    let pair = (1, -1);
    let (s, t) = pair;
    let mut best: i32 = 0;
    best = best.max(s * c - t * c);
    best
}

fn main() {
    println(f(3).to_string());
}
";
    let (fixed, clean, out) = fix_and_run("pair", whole);
    assert!(fixed.contains("let pair = (1i32, -1i32);"), "{fixed}");
    assert!(clean, "{fixed}");
    assert_eq!(out, "6\n");

    let field = "fn f(c: i32) -> i32 {
    let pair = (1, -1);
    let mut best: i32 = 0;
    best = best.max(pair.0 * c);
    best
}

fn main() {
    println(f(3).to_string());
}
";
    let (fixed, clean, out) = fix_and_run("field", field);
    assert!(fixed.contains("let pair = (1i32, -1);"), "{fixed}");
    assert!(clean, "{fixed}");
    assert_eq!(out, "3\n");
}

/// A literal that the target cannot hold is not offered: `-1` as a `u32`.
#[test]
fn a_tuple_position_literal_the_target_cannot_hold_is_not_offered() {
    let src = "fn f(c: u32) -> u32 {
    let pair = (-1, 2);
    c + pair.0
}

fn main() {
    println(f(3).to_string());
}
";
    let (dir, path) = fixture("neg", src);
    let out = karac()
        .arg("check")
        .arg("--output=json")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let json = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "{json}");
    assert!(!json.contains("-1u32"), "{json}");
}

/// The same through a tuple binding: `let x = r + pair.1; r = x;` names
/// `pair.1` and suffixes its literal.
#[test]
fn a_let_copying_a_tuple_binding_position_is_offered_its_literal_suffix() {
    let src = "fn f(steps: i64) -> i32 {
    let pair = (1, -1);
    let mut r: i32 = 10;
    for _ in 0..steps {
        let x = r + pair.1;
        r = x;
    }
    r
}

fn main() {
    println(f(3).to_string());
}
";
    let diag = check_text("pair-copy-diag", src);
    assert!(
        diag.contains("`pair.1` is an 'i64' because the literal written there"),
        "{diag}"
    );
    let (fixed, clean, out) = fix_and_run("pair-copy", src);
    assert!(fixed.contains("let pair = (1, -1i32);"), "{fixed}");
    assert!(clean, "{fixed}");
    assert_eq!(out, "7\n");
}
