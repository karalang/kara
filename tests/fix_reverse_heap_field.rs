//! B-2026-11-08-4 (kata 1500): Rust's `BinaryHeap<Reverse<T>>` written as a
//! type (a struct field) is the smallest-first `PriorityQueue[T]` once the
//! `Reverse` around the heap's items is dropped and the heap is built with
//! `PriorityQueue.new()`. `karac fix` used to offer the did-you-mean
//! `Release` (an atomic ordering) for the type's `Reverse`, leaving
//! `PriorityQueue[Release[i32]]`. Fixed by cdb638200 (kata 2008's
//! `BinaryHeap[Reverse[T]]`); this pins kata 1500's struct-field shape
//! through every `karac fix` round to a run.

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Every check's diagnostics, the text `karac fix` leaves, whether that
/// text checks clean, and what it prints under the MIR interpreter.
fn mend(dir: &Path, src: &str) -> (String, String, bool, String) {
    std::fs::create_dir_all(dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(&file, format!("{src}\n")).unwrap();
    let mut first = String::new();
    for _ in 0..5 {
        first += &karac(dir, &["check", "cell.kara"]).2;
        let before = std::fs::read_to_string(&file).unwrap();
        karac(dir, &["fix", "cell.kara"]);
        if std::fs::read_to_string(&file).unwrap() == before {
            break;
        }
    }
    let text = std::fs::read_to_string(&file).unwrap();
    let (clean, _, _) = karac(dir, &["check", "cell.kara"]);
    let (_, out, _) = karac(dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(dir);
    (first, text, clean, out)
}

#[test]
fn reverse_heap_field_type_becomes_the_item_type() {
    let src = "use std::collections::BinaryHeap;\nuse std::cmp::Reverse;\n\nstruct Pool {\n    free: BinaryHeap<Reverse<i32>>,\n}\n\nimpl Pool {\n    fn new() -> Self {\n        Pool { free: BinaryHeap::new() }\n    }\n\n    fn take(&mut self) -> i32 {\n        match self.free.pop() {\n            Some(Reverse(id)) => id,\n            None => -1,\n        }\n    }\n\n    fn give(&mut self, id: i32) {\n        self.free.push(Reverse(id));\n    }\n}\n\nfn main() {\n    let mut p = Pool::new();\n    p.give(3);\n    p.give(1);\n    p.give(2);\n    let a = p.take();\n    let b = p.take();\n    println(f\"{a} {b}\");\n}";
    let dir = std::env::temp_dir().join(format!("karac-reverse-heap-field-{}", std::process::id()));
    let (first, text, clean, out) = mend(&dir, src);
    assert!(!first.contains("Release"), "{first}");
    assert!(clean, "still refused after fix:\n{text}");
    assert!(
        text.contains("free: PriorityQueue[i32]")
            && text.contains("PriorityQueue.new()")
            && !text.contains("Reverse")
            && !text.contains("Release"),
        "{text}"
    );
    assert_eq!(out.trim_end(), "1 2", "{text}");
}
