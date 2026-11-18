//! B-2026-11-18-59: `karac fix` turns a Rust program's `usize`s into
//! `i64` once one meets an `i64` (design.md §5), and four methods of
//! `usize` floor at 0 where their `i64` namesakes do not: `checked_sub`
//! gave `Some(-1)` where Rust gave `None`, `saturating_sub` went below 0,
//! and `checked_add_signed` / `saturating_add_signed` are not methods of
//! `i64` at all. The fix keeps the floor in the round that retypes
//! (kata 1219's visited arm). A program whose `usize`s never meet an `i64`
//! keeps them, and its `checked_sub` with them.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("karac-usize-floor-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

/// `karac fix` until nothing is left, then the fixed text, whether it
/// checks clean, and what it prints on the interpreter.
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

/// Rust prints 2: `prev` at 0 is `None`, so -1. Before the fix the
/// fixed program checked clean and `__mir-run` panicked at `v[-1]`.
#[test]
fn a_usize_checked_sub_keeps_its_none_at_zero_once_the_usize_is_i64() {
    let src = "fn prev(v: &[i32], i: usize) -> i32 {
    match i.checked_sub(1) {
        Some(p) => v[p],
        None => -1,
    }
}

fn main() {
    let v = vec![1, 2, 3];
    let mut total = 0;
    for i in 0..v.len() {
        total += prev(&v, i);
    }
    println!(\"{}\", total);
}
";
    let (fixed, clean, out) = fix_and_run("sub", src);
    assert_eq!(
        fixed,
        "fn prev(v: Slice[i32], i: i64) -> i32 {
    match i.checked_sub(1).filter(|v| v >= 0) {
        Some(p) => v[p],
        None => -1,
    }
}

fn main() {
    let v = [1, 2, 3];
    let mut total = 0;
    for i in 0..v.len() {
        total += prev(v, i);
    }
    println(f\"{total}\");
}
"
    );
    assert!(clean, "{fixed}");
    assert_eq!(out, "2\n");
}

/// `saturating_sub` stops at 0 on a `usize`; the window at 0 and 1
/// starts at 0.
#[test]
fn a_usize_saturating_sub_keeps_its_floor_once_the_usize_is_i64() {
    let src = "fn window(v: &[i64], i: usize, k: usize) -> i64 {
    let from = i.saturating_sub(k);
    v[from..=i].iter().sum()
}

fn main() {
    let v = vec![5, 1, 4, 2];
    let mut out = Vec::new();
    for i in 0..v.len() {
        out.push(window(&v, i, 2));
    }
    println!(\"{} {} {} {}\", out[0], out[1], out[2], out[3]);
}
";
    let (fixed, clean, out) = fix_and_run("sat", src);
    assert_eq!(
        fixed,
        "fn window(v: Slice[i64], i: i64, k: i64) -> i64 {
    let from = i.saturating_sub(k).max(0);
    v[from..=i].iter().sum()
}

fn main() {
    let v = [5, 1, 4, 2];
    let mut out = Vec.new();
    for i in 0..v.len() {
        out.push(window(v, i, 2));
    }
    println(f\"{out[0]} {out[1]} {out[2]} {out[3]}\");
}
"
    );
    assert!(clean, "{fixed}");
    assert_eq!(out, "5 6 10 7\n");
}

/// Kata 1219's visited arm in small: `checked_add_signed` is not a method
/// of `i64`, so the fix was left with two errors and no edit.
#[test]
fn a_usize_checked_add_signed_becomes_i64_checked_add_with_the_floor() {
    let src = "const DIRS: [(isize, isize); 4] = [(-1, 0), (1, 0), (0, -1), (0, 1)];

fn neighbours(grid: &[Vec<i32>], i: usize, j: usize) -> i32 {
    let mut n = 0;
    for (di, dj) in DIRS {
        let (Some(ni), Some(nj)) = (i.checked_add_signed(di), j.checked_add_signed(dj)) else {
            continue;
        };
        if let Some(&g) = grid.get(ni).and_then(|row| row.get(nj)) {
            n += g;
        }
    }
    n
}

fn main() {
    let grid: Vec<Vec<i32>> = vec![vec![1, 2], vec![3, 4]];
    println!(\"{} {}\", neighbours(&grid, 0, 0), neighbours(&grid, 1, 1));
}
";
    let (fixed, clean, out) = fix_and_run("signed", src);
    assert_eq!(fixed, "const DIRS: Array[(i64, i64), 4] = [(-1, 0), (1, 0), (0, -1), (0, 1)];

fn neighbours(grid: Slice[Vec[i32]], i: i64, j: i64) -> i32 {
    let mut n: i32 = 0;
    for (di, dj) in DIRS {
        let (Some(ni), Some(nj)) = (i.checked_add(di).filter(|v| v >= 0), j.checked_add(dj).filter(|v| v >= 0)) else {
            continue;
        };
        if let Some(g) = grid.get(ni).and_then(|row| row.get(nj)).cloned() {
            n += g;
        }
    }
    n
}

fn main() {
    let grid: Vec[Vec[i32]] = [[1, 2], [3, 4]];
    println(f\"{neighbours(grid, 0, 0)} {neighbours(grid, 1, 1)}\");
}
");
    assert!(clean, "{fixed}");
    assert_eq!(out, "5 5\n");
}

/// The control: `usize`s that never meet an `i64` stay, and so does their
/// `checked_sub`, which floors as Rust's does.
#[test]
fn a_usize_checked_sub_in_a_program_that_keeps_its_usizes_is_left_alone() {
    let src = "fn prev(v: Slice[i32], i: usize) -> i32 {\n    match i.checked_sub(1) {\n        Some(p) => v[p],\n        None => -1,\n    }\n}\n\nfn main() {\n    let v = [1, 2, 3];\n    println(f\"{prev(v, 0)} {prev(v, 2)}\");\n}\n";
    let (fixed, clean, out) = fix_and_run("kept", src);
    assert_eq!(fixed, src);
    assert!(clean, "{fixed}");
    assert_eq!(out, "-1 2\n");
}
