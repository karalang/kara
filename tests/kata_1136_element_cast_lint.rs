//! Kata 1136's solution arm, written as LeetCode's Rust users write it,
//! builds its graph with `graph[r[0] as usize].push(r[1] as usize)` from
//! a `Vec<Vec<i32>>`. `karac fix` carried `usize` to `i64` and left
//! `graph[r[0] as i64].push(r[1] as i64)`: two casts for nothing, since a
//! list's position takes any integer and a `Vec[i64]`'s `push` widens an
//! `i32` itself. `lossless_cast` (design.md § `as` casts) flagged such a
//! widening at a binding, argument or return, but not an element pushed
//! or inserted, nor a position. It now flags both, and `karac fix` deletes
//! them (B-2026-11-12-5).

use std::process::{Command, Stdio};

fn karac(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kata_1136_element_cast_lint_{name}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `karac check`'s stderr, then the text `karac fix` leaves and what the
/// mended program prints.
fn check_and_mend(name: &str, src: &str) -> (String, String, String) {
    let dir = scratch(name);
    std::fs::write(dir.join("m.kara"), src).unwrap();
    let check = karac(&dir, &["check", "m.kara"]);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    for _ in 0..3 {
        karac(&dir, &["fix", "m.kara"]);
    }
    let text = std::fs::read_to_string(dir.join("m.kara")).unwrap();
    let run = karac(&dir, &["__mir-run", "m.kara"]);
    let out = String::from_utf8_lossy(&run.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&dir);
    (err, text, out)
}

#[test]
fn a_widening_cast_into_an_element_slot_is_flagged_and_deleted() {
    let src = "fn main() {
    let a: u8 = 1;
    let b: i16 = 2;
    let d: i32 = 1;
    let f: f32 = 1.5;
    let mut v: Vec[i64] = [];
    v.push(a as i64);
    let mut w: Vec[f64] = [];
    w.push(f as f64);
    let mut q: VecDeque[i64] = VecDeque.new();
    q.push_back(d as i64);
    let mut s: Set[i64] = Set.new();
    s.insert(b as i64);
    v.insert(0, d as i64);
    println(f\"{v} {w} {q} {s.len()}\");
}
";
    let (err, text, out) = check_and_mend("slots", src);
    for want in [
        ":7:12: unnecessary cast: 'u8' widens to 'i64' implicitly here",
        ":9:12: unnecessary cast: 'f32' widens to 'f64' implicitly here",
        ":11:17: unnecessary cast: 'i32' widens to 'i64' implicitly here",
        ":13:14: unnecessary cast: 'i16' widens to 'i64' implicitly here",
        ":14:17: unnecessary cast: 'i32' widens to 'i64' implicitly here",
    ] {
        assert!(err.contains(want), "missing {want:?} in:\n{err}");
    }
    assert_eq!(err.matches("_cast]").count(), 5, "{err}");
    for want in [
        "v.push(a);",
        "w.push(f);",
        "q.push_back(d);",
        "s.insert(b);",
        "v.insert(0, d);",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    assert_eq!(out, "[1, 1] [1.5] [1] 1\n", "{text}");
}

#[test]
fn a_widening_cast_in_a_list_position_is_flagged_and_deleted() {
    let src = "fn main() {
    let r: Vec[i32] = [0, 2];
    let mut graph: Vec[Vec[i64]] = Vec.filled(3, Vec.new());
    graph[r[0] as i64].push(r[1] as i64);
    let xs = [10, 20, 30];
    let k: u8 = 2;
    println(f\"{graph} {xs[k as i64]}\");
}
";
    let (err, text, out) = check_and_mend("position", src);
    assert_eq!(err.matches("_cast]").count(), 3, "{err}");
    assert!(text.contains("graph[r[0]].push(r[1]);"), "{text}");
    assert!(text.contains("{xs[k]}"), "{text}");
    assert_eq!(out, "[[2], [], []] 30\n", "{text}");
}

#[test]
fn a_cast_that_changes_the_value_or_a_map_key_is_left_alone() {
    let src = "fn main() {
    let big: u64 = 2;
    let wide: i64 = 300;
    let mut v: Vec[i64] = [];
    v.push(big as i64);
    let mut n: Vec[i32] = [];
    n.push(wide as i32);
    let mut m: Map[i32, i64] = Map.new();
    let k: i16 = 3;
    m[k as i32] = 1;
    let xs = [1, 2, 3];
    println(f\"{v} {n} {m.len()} {xs[big as i64]}\");
}
";
    let (err, text, out) = check_and_mend("kept", src);
    assert_eq!(err.matches("_cast]").count(), 0, "{err}");
    assert_eq!(text, src);
    assert_eq!(out, "[2] [300] 1 3\n", "{text}");
}
