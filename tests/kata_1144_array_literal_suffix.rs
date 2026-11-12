//! Found writing kata 1144's Rust arm (B-2026-11-12-16).
//!
//! LeetCode's Rust counts the moves for each parity in `let mut res =
//! [0, 0]`, adds `i32` amounts with `res[i % 2] += ...`, and returns
//! `min(res[0], res[1])` as the `i32` answer. Kāra types the array by its
//! first literal, which an operator leaves at the default `i64`, so the
//! return is a narrowing; `karac check` said only "write an explicit `as
//! i32`", where a fill (`Vec.filled(2, 0)`) or a lone literal is offered
//! its suffix. Now an array of literals bound whole offers its first
//! literal's suffix at a narrowing read of one of its elements.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-kata-1144-array-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

fn wrap(body: &str) -> String {
    format!("fn f() -> i32 {{\n    {body}\n}}\n\nfn main() {{\n    println(f\"{{f()}}\");\n}}\n")
}

/// `karac check`'s stderr, the edits `karac fix --dry-run` would make, and
/// what the program prints once fixed.
fn check_fix_run(tag: &str, body: &str) -> (String, String, String) {
    let path = temp(tag, &wrap(body));
    let check = karac(&["check"], &path);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    let dry = karac(&["fix", "--dry-run"], &path);
    let edits =
        String::from_utf8_lossy(&dry.stdout).into_owned() + &String::from_utf8_lossy(&dry.stderr);
    let _ = karac(&["fix"], &path);
    let run = karac(&["__mir-run"], &path);
    let out = String::from_utf8_lossy(&run.stdout).into_owned();
    let _ = std::fs::remove_file(&path);
    (err, edits, out)
}

#[test]
fn an_element_read_out_of_an_array_of_literals_is_fixed_on_its_first_literal() {
    let shapes = [
        (
            "read",
            "let v: Vec[i32] = [3, 4]; let mut r = [0, 0]; r[1] += v[0]; r[1]",
            "3\n",
        ),
        (
            "min",
            "let v: Vec[i32] = [3, 4, 5]; let mut res = [0, 0]; for i in 0..3 { res[i % 2] += v[i]; } min(res[0], res[1])",
            "4\n",
        ),
        (
            "assign",
            "let v: Vec[i32] = [3, 4]; let mut r = [0, 0]; r[0] = r[1] + v[1]; let k: i32 = r[0]; k",
            "4\n",
        ),
        (
            "copy",
            "let v: Vec[i32] = [3, 4]; let mut r = [0, 7]; r[0] = r[1] + v[1]; let k = r[0]; let mut m: i32 = 0; m = k; m",
            "11\n",
        ),
    ];
    for (tag, body, want) in shapes {
        let (err, edits, out) = check_fix_run(tag, body);
        assert!(
            err.contains("are 'i64' because the literals it is written with took the default")
                && err.contains("write the first of them with the suffix `i32`"),
            "{tag}: {err}"
        );
        assert!(edits.contains("→ `i32`"), "{tag}: {edits}");
        assert_eq!(out, want, "{tag}: {edits}");
    }
}

#[test]
fn an_array_of_literals_too_wide_for_the_target_is_offered_no_suffix() {
    let (err, edits, _) = check_fix_run(
        "wide",
        "let v: Vec[i32] = [3, 4]; let mut r = [0, 5000000000]; r[1] += v[0]; r[0]",
    );
    assert!(err.contains("as i32"), "{err}");
    assert!(!edits.contains("→ `i32`"), "{edits}");
}
