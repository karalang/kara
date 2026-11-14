//! Kata 2163 (Minimum Difference in Sums After Removal of Elements):
//! LeetCode's Rust template builds a plain `BinaryHeap` for the prefix and
//! then a `BinaryHeap` of `Reverse` items for the suffix, both bound to
//! `heap`, the second shadowing the first. Which way a heap pops was told
//! from every push of its name in the program, so the shadowed `heap` read
//! as both kinds and no heap or `Reverse` got a fix. A push now speaks for
//! the latest constructor of its name before it, and a `let`'s annotation
//! for the constructor in its own statement. Once `Reverse` is dropped from
//! the pushes, Rust's `heap.pop().unwrap().0` reads `.0` off the bare item:
//! that is refused with the fix that deletes it.

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

/// Every check's diagnostics, the text `karac fix` leaves after as many
/// rounds as change it, whether that text checks clean, and what it prints
/// under the MIR interpreter.
fn mend(tag: &str, src: &str) -> (String, String, bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-2163-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(&file, format!("{src}\n")).unwrap();
    let mut first = String::new();
    for _ in 0..8 {
        first += &karac(&dir, &["check", "cell.kara"]).2;
        let before = std::fs::read_to_string(&file).unwrap();
        karac(&dir, &["fix", "cell.kara"]);
        if std::fs::read_to_string(&file).unwrap() == before {
            break;
        }
    }
    let text = std::fs::read_to_string(&file).unwrap();
    let (clean, _, _) = karac(&dir, &["check", "cell.kara"]);
    let (_, out, _) = karac(&dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (first, text, clean, out)
}

/// The template's shape: one name, two heaps, one after the other.
#[test]
fn kata_2163_a_shadowed_heap_name_is_two_heaps() {
    let src = "use std::cmp::Reverse;\nuse std::collections::BinaryHeap;\n\nfn main() {\n    let mut heap = BinaryHeap::new();\n    for p in [5, 9, 1, 7] {\n        heap.push(p);\n    }\n    let a = heap.pop().unwrap();\n    let mut heap = BinaryHeap::new();\n    for p in [5, 9, 1, 7] {\n        heap.push(Reverse(p));\n    }\n    let b = heap.pop().unwrap().0;\n    println(f\"{a} {b}\");\n}";
    let (first, text, clean, out) = mend("shadowed", src);
    assert!(
        !first.contains("cannot be told"),
        "the two heaps read as one:\n{first}"
    );
    assert!(clean, "still refused after fix:\n{text}");
    assert_eq!(
        text.matches("let mut heap = PriorityQueue.max_first()")
            .count(),
        1,
        "{text}"
    );
    assert_eq!(
        text.matches("let mut heap = PriorityQueue.new()").count(),
        1,
        "{text}"
    );
    assert!(
        text.contains("heap.push(p);") && !text.contains("Reverse"),
        "{text}"
    );
    assert!(text.contains("let b = heap.pop().unwrap();"), "{text}");
    assert_eq!(out.trim_end(), "9 1", "{text}");
}

/// The same name in two functions, a plain heap in one and a `Reverse` one
/// in the other.
#[test]
fn kata_2163_one_heap_name_in_two_functions() {
    let src = "use std::cmp::Reverse;\nuse std::collections::BinaryHeap;\n\nfn most(xs: Vec<i64>) -> i64 {\n    let mut heap = BinaryHeap::new();\n    for x in xs {\n        heap.push(x);\n    }\n    *heap.peek().unwrap()\n}\n\nfn least(xs: Vec<i64>) -> i64 {\n    let mut heap = BinaryHeap::new();\n    for x in xs {\n        heap.push(Reverse(x));\n    }\n    heap.peek().unwrap().0\n}\n\nfn main() {\n    println(f\"{most(vec![5, 9, 1])} {least(vec![5, 9, 1])}\");\n}";
    let (_, text, clean, out) = mend("two-fns", src);
    assert!(clean, "still refused after fix:\n{text}");
    assert!(
        text.contains("let mut heap = PriorityQueue.max_first()")
            && text.contains("let mut heap = PriorityQueue.new()"),
        "{text}"
    );
    assert_eq!(out.trim_end(), "9 1", "{text}");
}

/// `.0` on what a `PriorityQueue` pops is refused with the fix that drops
/// it (and a later round reads the `peek`'s borrow with `*`); on what another collection pops it is refused as before, with none.
#[test]
fn kata_2163_tuple_index_on_a_queue_item_is_dropped() {
    let src = "fn main() {\n    let mut q: PriorityQueue[i64] = PriorityQueue.new();\n    q.push(4);\n    q.push(2);\n    let a = q.peek().unwrap().0;\n    let b = q.pop().expect(\"one\").0;\n    println(f\"{a} {b}\");\n}";
    let (first, text, clean, out) = mend("queue-dot0", src);
    assert!(
        first.contains("a `PriorityQueue` holds its items themselves"),
        "{first}"
    );
    assert!(clean, "still refused after fix:\n{text}");
    assert!(
        text.contains("let a = *q.peek().unwrap();")
            && text.contains("let b = q.pop().expect(\"one\");"),
        "{text}"
    );
    assert_eq!(out.trim_end(), "2 2", "{text}");
    let src = "fn main() {\n    let mut v: Vec[i64] = Vec.new();\n    v.push(4);\n    let a = v.pop().unwrap().0;\n    println(a);\n}";
    let (first, text, clean, _) = mend("vec-dot0", src);
    assert!(
        first.contains("tuple index on non-tuple type 'i64'") && !first.contains("PriorityQueue"),
        "{first}"
    );
    assert!(!clean && text.contains("v.pop().unwrap().0"), "{text}");
}
