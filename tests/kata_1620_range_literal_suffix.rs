//! Found writing kata 1620's Rust arm (B-2026-11-18-51).
//!
//! LeetCode's Rust walks the grid with `for x in 0..=50`, subtracts `x`
//! from an `i32` coordinate, and stores `vec![x, y]` as the `Vec<i32>`
//! answer. Kāra types a range of literals by its start, which an operator
//! leaves at the default `i64`, so each element of `[x, y]` (and a
//! `let k: i32 = x`) is a narrowing; `karac check` said only "write an
//! explicit `as i32`". Now a binding a range of literals feeds offers the
//! range's first literal's suffix (`0i32..=50`) at a narrowing read.

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
        "karac-kata-1620-range-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

/// `karac check`'s stderr, the file once `karac fix` ran, and what the
/// program prints then.
fn check_fix_run(tag: &str, src: &str) -> (String, String, String) {
    let path = temp(tag, src);
    let check = karac(&["check"], &path);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    let _ = karac(&["fix"], &path);
    let fixed = std::fs::read_to_string(&path).unwrap();
    let run = karac(&["__mir-run"], &path);
    let out =
        String::from_utf8_lossy(&run.stdout).into_owned() + &String::from_utf8_lossy(&run.stderr);
    let _ = std::fs::remove_file(&path);
    (err, fixed, out)
}

#[test]
fn a_range_of_literals_feeding_an_i32_element_is_fixed_on_its_start() {
    let src = "fn best(v: Slice[i32]) -> Vec[i32] {
    let mut out: Vec[i32] = [0, 0];
    for x in 0..=3 {
        for y in 0..3 {
            if v[0] - x == y {
                out = [x, y];
            }
        }
    }
    out
}

fn main() {
    println(best([3]));
}
";
    let (err, fixed, out) = check_fix_run("element", src);
    assert!(
        err.contains("`x` is an 'i64' because") && err.contains("write that literal `0i32`"),
        "{err}"
    );
    assert!(
        fixed.contains("for x in 0i32..=3 {") && fixed.contains("for y in 0i32..3 {"),
        "{fixed}"
    );
    assert_eq!(out, "[3, 0]\n");
}

#[test]
fn a_range_of_literals_feeding_an_annotated_let_is_fixed_on_its_start() {
    let src = "fn f(v: Slice[i32]) -> i32 {
    let mut s: i32 = 0;
    for x in 1..4 {
        let d = v[0] - x;
        let k: i32 = x;
        s += d * k;
    }
    s
}

fn main() {
    println(f([10]));
}
";
    let (err, fixed, out) = check_fix_run("let", src);
    assert!(err.contains("write that literal `1i32`"), "{err}");
    assert!(fixed.contains("for x in 1i32..4 {"), "{fixed}");
    assert_eq!(out, "46\n");
}
