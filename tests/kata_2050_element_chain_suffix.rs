//! Kata 2050's Rust arm: LeetCode's template keeps its finish months in
//! `let mut dist = vec![0; n];`, which `karac fix` spells
//! `Vec.filled(n, 0)`, and returns `*dist.iter().max().unwrap()` from a
//! function that returns `i32`. The literal took the default `i64`, and the
//! return was refused as a narrowing coercion with no fix: the fill's
//! suffix was offered only where the value read was `dist[i]`. A chain that
//! only walks the collection and picks one of its elements (`iter`, `max`,
//! `unwrap`, ...) reads an element too, so the fix is the same `0i32`
//! (design.md §8 Literal suffixes; B-2026-11-08-53). A `count()` of the
//! walk is no element, and gets no suffix.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path.to_str().unwrap().to_string()
}

#[test]
fn kata_2050_max_of_a_filled_vec_fixed_by_its_store_checks_as_written() {
    // `dist[i] = time[i] * 2` stores an `i32`, which fixes the fill's open
    // literal as an assignment fixes a binding's (B-2026-11-12-12; this
    // shape was refused while the store settled the literal at `i64`).
    let src = "fn latest(n: i64, time: Vec[i32]) -> i32 {
    let mut dist = Vec.filled(n, 0);
    for i in 0..n {
        dist[i] = time[i] * 2;
    }
    *dist.iter().max().unwrap()
}

fn main() {
    println(latest(3, [4, 9, 1]));
}
";
    let file = write("k2050-stored", src);
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let out = karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "18\n");
}

#[test]
fn kata_2050_max_of_a_filled_vec_returned_narrower_gets_the_fill_suffix() {
    // A print before the store reads the literal at its default.
    let src = "fn latest(n: i64, time: Vec[i32]) -> i32 {
    let mut dist = Vec.filled(n, 0);
    println(dist[0] + 0);
    for i in 0..n {
        dist[i] = time[i] * 2;
    }
    *dist.iter().max().unwrap()
}

fn main() {
    println(latest(3, [4, 9, 1]));
}
";
    let file = write("k2050-max", src);
    let check = karac(&["check", &file]);
    let msg = String::from_utf8_lossy(&check.stderr).into_owned();
    assert!(
        msg.contains("the elements of `dist` are 'i64' because the literal it is filled with took the default"),
        "{msg}"
    );
    let _ = karac(&["fix", &file]);
    let fixed = std::fs::read_to_string(&file).unwrap();
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    assert!(
        check.status.success(),
        "{}\n{fixed}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(fixed.contains("Vec.filled(n, 0i32)"), "{fixed}");
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "0\n18\n");
}

#[test]
fn kata_2050_count_of_a_filled_vec_gets_no_fill_suffix() {
    let src = "fn zeros(n: i64) -> i32 {
    let xs = Vec.filled(n, 0);
    xs.iter().filter(|x| x == 0).count()
}

fn main() {
    println(zeros(3));
}
";
    let file = write("k2050-count", src);
    let check = karac(&["check", &file]);
    let dry = karac(&["fix", "--dry-run", &file]);
    let _ = std::fs::remove_file(&file);
    let msg = String::from_utf8_lossy(&check.stderr).into_owned();
    assert!(msg.contains("would narrow"), "{msg}");
    let plan = String::from_utf8_lossy(&dry.stdout).into_owned();
    assert!(!plan.contains("`i32`"), "{plan}");
}
