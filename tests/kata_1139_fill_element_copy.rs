//! Found writing kata 1139's Rust arm (B-2026-11-12-11).
//!
//! LeetCode's Rust keeps `left` and `up`, `vec![vec![0; n + 1]; m + 1]`
//! tables of runs, reads `let max_k = min(left[i][j], up[i][j])`, and
//! stores `res = k` for `k` in `(1..=max_k).rev()` with `res: i32`. After
//! `karac fix` the tables are `Vec.filled(m + 1, Vec.filled(n + 1, 0))`,
//! whose literal an operator settles to `i64`, and `res = k` was refused
//! with only "write an explicit `as i32`": the fill's literal was offered
//! its suffix (B-2026-11-08-57) only at a read of the fill itself, one
//! index deep. Now a fill of fills is read to its innermost literal, a
//! binding holding an element (`let k = g[i][j]`, through `min`/`max` or a
//! range's loop variable) is traced to it, and an assignment's value is
//! open for the place it is stored in, as a typed `let`'s is.

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
        "karac-kata-1139-fill-{tag}-{}.kara",
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
fn an_element_copied_out_of_a_settled_fill_is_fixed_on_the_fill() {
    let shapes = [
        (
            "let",
            "let mut t = Vec.filled(3, 0); t[1] = t[0] + 7; let k = t[1]; let r: i32 = k; r",
        ),
        (
            "max",
            "let mut t = Vec.filled(3, 0); t[1] = t[0] + 7; let k = max(t[1], t[0]); let r: i32 = k; r",
        ),
        (
            "range",
            "let mut t = Vec.filled(3, 0); t[1] = t[0] + 7; let mut r: i32 = 0; for k in (1..=t[1]).rev() { if k > r { r = k; } } r",
        ),
        (
            "assign",
            "let mut t = Vec.filled(3, 0); t[1] = t[0] + 7; let mut r: i32 = 0; let k = t[1]; r = k; r",
        ),
    ];
    for (tag, body) in shapes {
        let (err, edits, out) = check_fix_run(tag, body);
        assert!(
            err.contains("the elements of `t` are 'i64' because the literal it is filled with"),
            "{tag}: {err}"
        );
        assert!(edits.contains("`` → `i32`"), "{tag}: {edits}");
        assert_eq!(out, "7\n", "{tag}");
    }
}

#[test]
fn a_fill_of_fills_is_read_to_its_innermost_literal() {
    // Settled by an operator: the suffix goes on the inner `0`.
    let (err, edits, out) = check_fix_run(
        "nested",
        "let mut g = Vec.filled(2, Vec.filled(3, 0)); g[1][2] = g[0][0] + 9; let r: i32 = g[1][2]; r",
    );
    assert!(err.contains("the elements of `g` are 'i64'"), "{err}");
    assert!(edits.contains(":2:46: `` → `i32`"), "{edits}");
    assert_eq!(out, "9\n");
    // Open: the typed read fixes it, as a flat fill's.
    let path = temp(
        "nested_open",
        &wrap("let g = Vec.filled(2, Vec.filled(3, 5)); let r: i32 = g[1][2]; r"),
    );
    let check = karac(&["check"], &path);
    let run = karac(&["__mir-run"], &path);
    let _ = std::fs::remove_file(&path);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "5\n");
}

#[test]
fn an_assigned_element_read_is_open_for_the_place_it_is_stored_in() {
    // As `let r: i32 = t[1]` is accepted, so is `r = t[1]`.
    let path = temp(
        "assigned",
        &wrap("let t = Vec.filled(3, 4); let mut r: i32 = 0; r = t[1]; r"),
    );
    let check = karac(&["check"], &path);
    let run = karac(&["__mir-run"], &path);
    let _ = std::fs::remove_file(&path);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "4\n");
}
