//! Kata 1131's solution arm, written as LeetCode's Rust users write it,
//! walks the four sign pairs as `for &(p, q) in &[(1, 1), (1, -1), ..]`
//! and sums `p * a + q * b + i` over `i32` values. Two `karac fix` gaps
//! stood between it and Kāra. `&[..]` was rewritten to `ref [..]`, which
//! the checker then refuses, since a list literal is a new value and no
//! place to borrow: the `&` now goes (B-2026-11-12-3). And a value that
//! was an `i64` because of two literal bindings, `p * a + q * b` with
//! `let p = 1; let q = 2;`, had no fix at all: the first binding is now
//! typed, and a later round types the next (B-2026-11-12-4).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("kata_1131_fix_gaps_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The text `karac fix` leaves once it has nothing left to apply, whether
/// `karac check` then passes, and what the program prints.
fn mend(name: &str, src: &str) -> (String, bool, String) {
    let dir = scratch(name);
    let file = dir.join("m.kara");
    std::fs::write(&file, src).unwrap();
    for _ in 0..6 {
        karac(&dir, &["fix", "m.kara"]);
    }
    let text = std::fs::read_to_string(&file).unwrap();
    let (clean, _) = karac(&dir, &["check", "m.kara"]);
    let (_, out) = karac(&dir, &["__mir-run", "m.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (text, clean, out)
}

#[test]
fn a_borrowed_list_of_sign_pairs_is_walked_as_it_is() {
    let src = "fn main() {
    let mut best = 0;
    for &(p, q) in &[(1, 1), (1, -1), (-1, 1), (-1, -1)] {
        best = best.max(p * 3 + q * 4);
    }
    println(f\"{best}\");
}
";
    let (text, clean, out) = mend("pairs", src);
    assert!(
        text.contains("for (p, q) in [(1, 1), (1, -1), (-1, 1), (-1, -1)]"),
        "{text}"
    );
    assert!(!text.contains("ref ["), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "7\n", "{text}");
}

#[test]
fn a_borrowed_list_or_repeat_literal_loses_its_borrow() {
    let src = "fn main() {
    let mut total = 0;
    for x in &[1, 2, 3] {
        total += x;
    }
    for z in &[5; 2] {
        total += z;
    }
    println(f\"{total}\");
}
";
    let (text, clean, out) = mend("lists", src);
    assert!(text.contains("for x in [1, 2, 3]"), "{text}");
    assert!(text.contains("for z in [5; 2]"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "16\n", "{text}");
}

#[test]
fn a_value_wide_because_of_two_literal_bindings_types_each_in_turn() {
    let src = "fn main() {
    let a: i32 = 3;
    let mut m: i32 = 0;
    let p = 1;
    let q = 2;
    let v = p * a + q * a;
    m = m.max(v);
    println(f\"{m}\");
}
";
    let (text, clean, out) = mend("lets", src);
    assert!(text.contains("let p: i32 = 1;"), "{text}");
    assert!(text.contains("let q: i32 = 2;"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "9\n", "{text}");
}

#[test]
fn a_value_wide_because_of_a_walked_tuple_list_types_its_literals() {
    let src = "fn main() {
    let a: i32 = 3;
    let mut m: i32 = 0;
    for (p, q) in [(1, 1), (1, -1)] {
        let v = p * a + q;
        m = m.max(v);
    }
    println(f\"{m}\");
}
";
    let (text, clean, out) = mend("walk", src);
    assert!(text.contains("[(1i32, 1i32), (1, -1)]"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "4\n", "{text}");
}
