//! Kata 1111's iterator arm carries the nesting depth with Rust's `scan`,
//! writing the state inside an `if`: `.scan(0, |depth, c| { let group =
//! if c == '(' { *depth += 1; (*depth - 1) % 2 } else { *depth -= 1;
//! *depth % 2 }; Some(group) })`, collected into a `Vec[i32]`. Three gaps
//! stood between it and a clean check:
//!
//! - a write of the state below the body's own statements withheld the
//!   rewrite to Kāra's `Some((state, item))`; it now copies the state
//!   with `let mut depth = depth;` and writes the copy;
//! - the state's literal `0` took the default `i64`, so the items did
//!   too, and collecting them into a `Vec[i32]` had no fix; the fix is
//!   `0i32`;
//! - `match v.last() { Some(g) => g, None => 0 }` over a `Vec[i32]`
//!   was refused as `ref i32` against `i64`; the literal now takes the
//!   number the `ref` reads as, as it does beside an owned `i32`.

use std::process::{Command, Stdio};

/// `src` after `karac fix` until nothing changes, and its `check` stderr.
fn fixed(tag: &str, src: &str) -> (String, String) {
    let path =
        std::env::temp_dir().join(format!("karac-kata1111-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    for _ in 0..6 {
        let before = std::fs::read_to_string(&path).unwrap();
        let _ = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(["fix", path.to_str().unwrap()])
            .stdin(Stdio::null())
            .output()
            .expect("karac");
        if std::fs::read_to_string(&path).unwrap() == before {
            break;
        }
    }
    let check = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let out = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    (out, String::from_utf8_lossy(&check.stderr).into_owned())
}

fn clean(check: &str) -> bool {
    check.contains("All checks passed") || check.is_empty()
}

#[test]
fn kata_1111_a_scan_writing_its_state_in_a_branch_is_rewritten() {
    let src = "fn split(seq: Str) -> Vec[i32] {
    seq.chars()
        .scan(0, |depth, c| {
            let group = if c == '(' {
                *depth += 1;
                (*depth - 1) % 2
            } else {
                *depth -= 1;
                *depth % 2
            };
            Some(group)
        })
        .collect()
}

fn main() {
    println(f\"{split(\"(()())\")} {split(\"((()))\")}\");
}
";
    let (text, check) = fixed("nested", src);
    assert!(text.contains(".scan(0i32, |depth, c| {"), "{text}");
    assert!(text.contains("let mut depth = depth;"), "{text}");
    assert!(text.contains("depth += 1;"), "{text}");
    assert!(text.contains("Some((depth, group))"), "{text}");
    assert!(!text.contains("*depth"), "{text}");
    assert!(clean(&check), "{check}\n{text}");
    let out = karac::mir::lower::run_source(&text)
        .unwrap_or_else(|e| panic!("{e}\n{text}"))
        .output;
    assert_eq!(out, "[0, 1, 1, 1, 1, 0] [0, 1, 0, 0, 1, 0]\n");
}

#[test]
fn kata_1111_a_scan_whose_literal_state_feeds_narrow_items_gets_the_suffix() {
    let src = "fn main() {
    let v = [1, 2, 3];
    let sums: Vec[i32] = v.iter().scan(0, |s, x| Some((s + 1, s))).collect();
    println(f\"{sums}\");
}
";
    let path =
        std::env::temp_dir().join(format!("karac-kata1111-suffix-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let check = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&check.stderr);
    assert!(stderr.contains("write `0i32`"), "{stderr}");
    let (text, check) = fixed("suffix", src);
    assert!(text.contains(".scan(0i32, "), "{text}");
    assert!(clean(&check), "{check}\n{text}");
    let out = karac::mir::lower::run_source(&text)
        .unwrap_or_else(|e| panic!("{e}\n{text}"))
        .output;
    assert_eq!(out, "[0, 1, 2]\n");
}

#[test]
fn kata_1111_a_literal_arm_takes_the_number_a_ref_arm_reads_as() {
    let src = "fn main() {
    let v: Vec[i32] = [4, 7];
    let e: Vec[i32] = [];
    let a = match v.last() { Some(g) => g, None => 0 };
    let b = if let Some(g) = e.last() { g } else { 0 };
    let c = if let Some(g) = v.first() { g } else { 0 };
    let d = match e.first() { None => 5, Some(g) => g };
    let total: i32 = a + b + c + d;
    println(f\"{a} {b} {c} {d} {total}\");
}
";
    let (text, check) = fixed("refarm", src);
    assert_eq!(text, src, "nothing to fix");
    assert!(clean(&check), "{check}");
    let out = karac::mir::lower::run_source(src)
        .unwrap_or_else(|e| panic!("{e}"))
        .output;
    assert_eq!(out, "7 0 4 5 16\n");
}
