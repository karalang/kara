//! A bare (borrowed) parameter handed to an owned slot of a GENERIC
//! signature, `PriorityQueue.from(v)` or `take[T](v: own Vec[T])` with
//! `v: Vec[i64]`, is reported as the parameter given away, with the fix that
//! writes `own` on it, as it is for a plain `own Vec[i64]` slot, rather than
//! "expected 'Vec[T]', found 'ref Vec[i64]'" and an unsolved `T` with no fix
//! (B-2026-11-04-78, kata 1046). A slot that borrows (`ref Vec[T]`, a
//! `Slice[T]`, a bare `T`) still takes the borrow.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

#[test]
fn karac_fix_owns_a_parameter_a_generic_owned_slot_takes() {
    let src = "fn take[T](v: own Vec[T]) -> i64 {
    v.len()
}
fn heap(v: Vec[i64]) -> i64 {
    let mut q = PriorityQueue.max_first_from(v);
    q.pop().unwrap()
}
fn queue(v: Vec[i64]) -> i64 {
    let q = PriorityQueue.from(v);
    q.len()
}
fn taken(v: Vec[i64]) -> i64 {
    take(v)
}
fn id[T](x: T) -> i64 {
    1
}
fn size[T](v: ref Vec[T]) -> i64 {
    v.len()
}
fn first[T](v: Slice[T]) -> i64 {
    v.len()
}
fn lent(v: Vec[i64]) -> i64 {
    id(v) + size(v) + first(v)
}
fn main() {
    println(f\"{heap([2, 7, 4])} {queue([1, 2])} {taken([5])} {lent([1, 2, 3])}\");
}
";
    let path = std::env::temp_dir().join(format!(
        "karac-generic-own-slot-{}.kara",
        std::process::id()
    ));
    let file = path.to_str().unwrap();
    std::fs::write(&path, src).unwrap();

    let check = karac(&["check", file]);
    let err = String::from_utf8_lossy(&check.stderr);
    assert!(!err.contains("found 'ref Vec"), "{err}");
    assert!(!err.contains("cannot infer type parameter"), "{err}");
    for want in [
        ":5:46: cannot give away parameter `v`",
        ":9:32: cannot give away parameter `v`",
        ":13:10: cannot give away parameter `v`",
    ] {
        assert!(err.contains(want), "missing {want:?} in:\n{err}");
    }
    assert_eq!(err.matches("cannot give away").count(), 3, "{err}");

    let fix = karac(&["fix", file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let out = std::fs::read_to_string(&path).unwrap();
    for want in [
        "fn heap(v: own Vec[i64])",
        "fn queue(v: own Vec[i64])",
        "fn taken(v: own Vec[i64])",
        "fn lent(v: Vec[i64])",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        (
            run.status.code(),
            String::from_utf8_lossy(&run.stdout).as_ref()
        ),
        (Some(0), "7 2 1 7\n"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
