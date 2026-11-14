//! Kata 2135's differential handed a field through a loop's `ref` binding
//! to a closure passed to `map` and collected at once,
//! `(0..6).map(|arm| answer(arm, p.starts, p.targets)).collect()`, and
//! then passed the same field to a closure that borrows its argument,
//! `turn(p.starts)`. The checker refused the second: "cannot consume
//! `p.starts` while closure at .. captures `p.starts` by `ref` (borrow
//! still live)". Both halves of that were wrong. The first closure was gone
//! once `collect` returned, since the `map` iterator holding it was a
//! temporary that `collect` drained; and a call through a closure binding
//! whose parameter borrows does not consume its argument. Each half alone
//! was enough to refuse a correct program, so each has its own test here,
//! and a closure that really is still alive keeps its borrow.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

/// Writes `src`, runs `karac check` and `karac __mir-run` on it, and
/// returns (check's stderr, whether check passed, the run's stdout).
fn check_and_run(tag: &str, src: &str) -> (String, bool, String) {
    let path = std::env::temp_dir().join(format!("karac-k2135-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    (
        String::from_utf8_lossy(&check.stderr).into_owned()
            + &String::from_utf8_lossy(&check.stdout),
        check.status.success(),
        String::from_utf8_lossy(&run.stdout).into_owned(),
    )
}

#[test]
fn kata_2135_collected_map_closure_releases_its_field_capture() {
    // `t`'s parameter borrows and `f`'s does too, so only the release at
    // `collect` is in play: before it, `f(p.v)` inside the closure kept
    // `p.v` borrowed to the end of the loop body.
    let (diag, ok, out) = check_and_run(
        "collect",
        "struct P {
    v: Vec[i64],
}

fn f(x: Vec[i64]) -> i64 {
    x.len()
}

fn g(x: own Vec[i64]) -> i64 {
    x.len() * 10
}

fn main() {
    let mut ps: Vec[P] = Vec.new();
    ps.push(P { v: [1, 2].to_vec() });
    let p = P { v: [4, 5, 6].to_vec() };
    for q in ps.iter() {
        let a: Vec[i64] = (0..2).map(|i| f(q.v) + i).collect();
        let s: i64 = (0..2).map(|i| f(q.v) * i).sum();
        println(f\"{a} {s} {f(q.v)}\");
    }
    let b: Vec[i64] = (0..2).rev().map(|i| f(p.v) + i).collect();
    println(f\"{b} {g(p.v)}\");
}
",
    );
    assert!(ok, "{diag}");
    assert_eq!(out, "[2, 3] 2 2\n[4, 3] 30\n");
}

#[test]
fn kata_2135_closure_call_borrowing_its_argument_does_not_consume_it() {
    // `h` is alive across `t(p.v)`, so only the argument's mode is in play.
    let (diag, ok, out) = check_and_run(
        "call",
        "struct P {
    v: Vec[i64],
}

fn f(x: Vec[i64]) -> i64 {
    x.len()
}

fn main() {
    let p = P { v: [1, 2].to_vec() };
    let h = |a: i64| f(p.v) + a;
    let t = |list: Vec[i64]| list.len();
    println(f\"{t(p.v)} {h(1)} {p.v}\");
}
",
    );
    assert!(ok, "{diag}");
    assert_eq!(out, "2 3 [1, 2]\n");
}

#[test]
fn kata_2135_live_closure_still_holds_its_field_capture() {
    // An iterator kept in a binding keeps its closure, so moving the field
    // the closure reads while the iterator is still to be drained is
    // refused as before.
    let (diag, ok, _) = check_and_run(
        "live",
        "struct P {
    v: Vec[i64],
}

fn f(x: Vec[i64]) -> i64 {
    x.len()
}

fn g(x: own Vec[i64]) -> i64 {
    x.len()
}

fn main() {
    let p = P { v: [1, 2].to_vec() };
    let it = (0..2).map(|i| f(p.v) + i);
    let n = g(p.v);
    let a: Vec[i64] = it.collect();
    println(f\"{a} {n}\");
}
",
    );
    assert!(!ok, "a move under a live capture was accepted");
    assert!(
        diag.contains("cannot consume `p.v` while closure"),
        "{diag}"
    );
}
