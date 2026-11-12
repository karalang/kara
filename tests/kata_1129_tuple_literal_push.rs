//! Kata 1129's solution arm, written as LeetCode's Rust users write it,
//! keeps a `VecDeque` of `(node, length, last colour)` and pushes
//! `(next, d + 1, color)`, `color` read out of an adjacency list whose
//! colours were pushed as the literals `0` and `1`. A number pushed alone
//! takes the element's type, but one inside a pushed tuple kept its
//! default: `let x = 0; q.push((x, 1))` into a `Vec[(i32, i32)]` was
//! refused as a narrowing of `x` from `i64`, though an argument fixes a
//! literal (design.md §8 Literals). Each open number in a pushed tuple now
//! takes its slot's type (B-2026-11-12-1).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kata_1129_tuple_literal_push_{name}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Whether `karac __mir-run` succeeded, its stdout, and its stderr.
fn run(name: &str, src: &str) -> (bool, String, String) {
    let dir = scratch(name);
    std::fs::write(dir.join("m.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", "m.kara"])
        .current_dir(Path::new(&dir))
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn an_open_number_in_a_pushed_tuple_takes_its_slot_type() {
    let src = "fn main() {
    let x = 0;
    let y = 2;
    let z = 7;
    let mut v: Vec[(i32, i32)] = [];
    v.push((x, 1));
    let mut q: VecDeque[(i64, i32, u8)] = VecDeque.new();
    q.push_back((x, y, 3));
    let mut w: Vec[((i16, u8), f32)] = [];
    w.push(((z, 5), 0.5));
    let mut u = Vec.new();
    u.push(((1, 2), 3));
    let uu: Vec[((u8, i16), i32)] = u;
    println(f\"{v} {q.len()} {w} {uu}\");
}
";
    let (ok, out, err) = run("slots", src);
    assert!(ok, "{err}");
    assert_eq!(out, "[(0, 1)] 1 [((7, 5), 0.5)] [((1, 2), 3)]\n");
}

#[test]
fn a_number_read_back_from_a_pushed_tuple_takes_a_later_slot_type() {
    let src = "fn main() {
    let mut graph = Vec.filled(2, Vec.new());
    graph[0].push((1, 0));
    graph[0].push((1, 1));
    let mut q: VecDeque[(i64, i32)] = VecDeque.new();
    for (next, color) in ref graph[0] {
        q.push_back((next, color));
    }
    let mut seen: Vec[(i64, i32)] = [];
    while let Some(t) = q.pop_front() {
        seen.push(t);
    }
    println(f\"{seen}\");
}
";
    let (ok, out, err) = run("graph", src);
    assert!(ok, "{err}");
    assert_eq!(out, "[(1, 0), (1, 1)]\n");
}

#[test]
fn a_fixed_wide_value_in_a_pushed_tuple_is_still_refused() {
    let src = "fn main() {
    let x = 0;
    let big: i64 = 5;
    let mut v: Vec[(i32, i32)] = [];
    v.push((x, big));
    println(f\"{v}\");
}
";
    let (ok, _, err) = run("wide", src);
    assert!(!ok);
    assert!(
        err.contains("implicit coercion from 'i64' to 'i32' would narrow"),
        "{err}"
    );
    assert!(err.contains("5:16"), "{err}");
}
