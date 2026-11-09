//! Kata 2054's Rust arm: LeetCode's template drains a min-heap with
//! `while let Some(&Reverse((end, val))) = heap.peek()` before its first
//! `heap.push(Reverse((e[1], e[2])))`, and `karac fix` carries it to
//! `while let Some((end, val)) = heap.peek().cloned()` over
//! `PriorityQueue.new()`. The queue's element type is fixed only by the
//! later push, and the tuple pattern over that still-open type bound its
//! leaves to nothing: `karac check` passed, and the MIR builder refused the
//! program with "a type that failed to check is not lowered yet". A tuple
//! pattern over an open type now makes it a tuple of open parts, which the
//! push solves, as Rust's inference does; and `best.max(val)` with `best:
//! i32` takes `val`'s open type to be `i32`.

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

fn checks_and_prints(name: &str, src: &str, expected: &str) {
    let file = write(name, src);
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
    assert_eq!(out, expected);
}

const HEAP: &str = "fn main() {
    let rows: Vec[Vec[i32]] = [[1, 5], [2, 3], [7, 9]];
    let mut heap = PriorityQueue.new();
    let mut best: i32 = 0;
    for e in ref rows {
        while let Some((end, val)) = heap.peek().cloned() {
            if end < e[0] {
                best = best.max(val);
                heap.pop();
            } else {
                break;
            }
        }
        heap.push((e[1], e[0]));
    }
    println(f\"{best}\");
}
";

#[test]
fn kata_2054_tuple_pattern_over_a_heap_pushed_later_checks_and_runs() {
    checks_and_prints("k2054-cloned", HEAP, "2\n");
}

#[test]
fn kata_2054_tuple_pattern_over_a_borrowed_peek_checks_and_runs() {
    let src = HEAP.replace("heap.peek().cloned()", "heap.peek()");
    checks_and_prints("k2054-peek", &src, "2\n");
}

#[test]
fn kata_2054_tuple_pattern_against_a_later_scalar_is_a_type_error() {
    let src = "fn main() {
    let mut v = Vec.new();
    if let Some((a, b)) = v.last() {
        println(f\"{a} {b}\");
    }
    v.push(3);
}
";
    let file = write("k2054-scalar", src);
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    let msg = String::from_utf8_lossy(&check.stderr).into_owned();
    assert!(!check.status.success(), "{msg}");
    assert!(!msg.contains("not lowered yet"), "{msg}");
}
