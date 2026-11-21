//! Found writing kata 2737 (Find the Closest Marked Node).
//!
//! B-2026-11-20-139: `queue.insert((0, v))` into a `SortedSet[(i64, i64)]`, with `v`
//! a `for` element over a `Vec[i64]` (a `ref i64`), was refused with
//! "expected '(i64, i64)', found '(i64, ref i64)'", while `Vec.push` and
//! `PriorityQueue.push` took the same tuple and copied `v` in. The set
//! arms compared the argument's type before resolving the variable the
//! literal `0` took its slot's type through; they now compare it resolved,
//! as `push` does. A borrowed `String` in the same place is still refused.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn check(src: &str) -> String {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "karac-set-insert-tuple-ref-{}-{n}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

const COPY_INTO_SETS: &str = "fn main() {
    let marked: Vec[i64] = [3, 1, 3].to_vec();
    let mut sorted: SortedSet[(i64, i64)] = SortedSet.new();
    let mut hashed: Set[(i64, i64)] = Set.new();
    for v in marked.iter() {
        sorted.insert((0, v));
        hashed.insert((v, 7));
    }
    while let Some((d, u)) = sorted.pop_first() {
        println(f\"{d} {u}\");
    }
    println(f\"{hashed.len()} {hashed.contains((1, 7))}\");
}
";

const BORROWED_STRING: &str = "fn main() {
    let names: Vec[String] = [\"a\".to_string()].to_vec();
    let mut s: SortedSet[(i64, String)] = SortedSet.new();
    for v in names.iter() {
        s.insert((0, v));
    }
    println(f\"{s.len()}\");
}
";

const WRONG_TYPE: &str = "fn main() {
    let mut s: Set[(i64, i64)] = Set.new();
    s.insert((0, \"x\"));
    println(f\"{s.len()}\");
}
";

#[test]
fn set_insert_tuple_ref_copy_checks_clean() {
    let out = check(COPY_INTO_SETS);
    assert!(out.contains("All checks passed"), "{out}");
}

#[test]
fn set_insert_tuple_ref_copy_runs() {
    assert_eq!(run(COPY_INTO_SETS), "0 1\n0 3\n2 true\n");
}

#[test]
fn set_insert_tuple_ref_string_still_refused() {
    let out = check(BORROWED_STRING);
    assert!(
        out.contains("expected '(i64, String)', found '(i64, ref String)'"),
        "{out}"
    );
}

#[test]
fn set_insert_tuple_wrong_type_still_refused() {
    let out = check(WRONG_TYPE);
    assert!(
        out.contains("expected '(i64, i64)', found '(i64, String)'"),
        "{out}"
    );
}
