//! B-2026-11-21-70, the remainder of B-2026-11-18-59: `karac fix` kept a `usize`'s
//! floor at 0 only for a `saturating_sub` / `checked_sub` whose RECEIVER
//! was a written `usize`. Over an index (`for i in 0..v.len()`, an `i64` in
//! Kāra) with a written `usize` argument (`i.saturating_sub(k)` with
//! `k: usize`), Rust's receiver was a `usize` too, since its operands are
//! one type; the retype to `i64` left the call alone and the fixed program
//! indexed below 0. Kata 1314's and 1326's Rust arms.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("karac-usize-arg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
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

/// Rust prints 1 + 1 + 1 + 2 = 5: at `i` 0, 1 and 2 the start clamps to 0.
#[test]
fn a_usize_argument_keeps_the_floor_of_an_index_receiver() {
    let src = "fn window_starts(v: &Vec<i32>, k: i32) -> i32 {
    let k = k as usize;
    let mut s = 0;
    for i in 0..v.len() {
        s += v[i.saturating_sub(k)];
    }
    s
}

fn main() {
    let v = vec![1, 2, 3, 4];
    println!(\"{}\", window_starts(&v, 2));
}
";
    let (fixed, clean, out) = fix_and_run("let", src);
    assert!(fixed.contains("i.saturating_sub(k).max(0)"), "{fixed}");
    assert!(clean, "{fixed}");
    assert_eq!(out, "5\n");
}

/// The same with the `usize` a parameter.
#[test]
fn a_usize_parameter_argument_keeps_the_floor_of_an_index_receiver() {
    let src = "fn window_starts(v: &Vec<i32>, k: usize) -> i32 {
    let mut s = 0;
    for i in 0..v.len() {
        s += v[i.saturating_sub(k)];
    }
    s
}

fn main() {
    let v = vec![1, 2, 3, 4];
    println!(\"{}\", window_starts(&v, 2));
}
";
    let (fixed, clean, out) = fix_and_run("param", src);
    assert!(fixed.contains("i.saturating_sub(k).max(0)"), "{fixed}");
    assert!(clean, "{fixed}");
    assert_eq!(out, "5\n");
}

/// `checked_sub` over the same pair: `None` at the start, as in Rust.
#[test]
fn a_usize_argument_keeps_the_none_of_an_index_checked_sub() {
    let src = "fn back(v: &Vec<i32>, k: usize) -> i32 {
    let mut s = 0;
    for i in 0..v.len() {
        if let Some(j) = i.checked_sub(k) {
            s += v[j];
        }
    }
    s
}

fn main() {
    let v = vec![1, 2, 3, 4];
    println!(\"{}\", back(&v, 2));
}
";
    let (fixed, clean, out) = fix_and_run("checked", src);
    assert!(
        fixed.contains("i.checked_sub(k).filter(|v| v >= 0)"),
        "{fixed}"
    );
    assert!(clean, "{fixed}");
    assert_eq!(out, "3\n");
}
