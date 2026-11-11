//! Two Rust spellings kata 1057's arms met, carried by `karac fix` to Kāra.
//!
//! - A `usize` written in a type that then mixes with `i64` (a push, a
//!   compare, a store) is rewritten to `i64`: Kāra keeps `usize` and `isize`
//!   for FFI signatures and writes every size, length, count and index as
//!   `i64` (design.md §5, B-2026-11-06-3). A `usize` that mixes with nothing
//!   draws nothing.
//! - Rust's `std::cmp::Reverse` around a `BinaryHeap`'s items, which makes
//!   the heap pop the smallest first, is dropped, and the heap is built as
//!   `PriorityQueue.new()`, which pops the smallest first already. As a
//!   sort key's whole body it becomes the descending `sort_by` (kata 1058);
//!   anywhere else it is `std.cmp.Reverse` (tests/std_cmp_reverse.rs), and
//!   never the did-you-mean `Release` (kata 1057).

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
    // Every check's diagnostics, since the resolver's only appear once the
    // parse's fixes are in.
    let mut first = String::new();
    for _ in 0..4 {
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

fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("karac-kata-1057-{name}-{}", std::process::id()))
}

#[test]
fn kata_1057_usize_mixed_with_i64_becomes_i64() {
    let src = "fn main() {\n    let mut places: Vec[Vec[usize]] = Vec.filled(3, Vec.new());\n    for (i, c) in \"abca\".bytes().enumerate() {\n        places[c - 97].push(i);\n    }\n    let pos = 1;\n    let k = places[0].partition_point(|p| p < pos);\n    let next: i64 = places[0][k] + 1;\n    println(f\"{places} {k} {next}\");\n}";
    let (first, text, clean, out) = mend(&scratch("usize"), src);
    assert!(
        first.contains("`usize` and `isize` are for FFI signatures"),
        "no usize hint on the first check:\n{first}"
    );
    assert!(clean, "still refused after fix:\n{text}");
    assert!(
        text.contains("Vec[Vec[i64]]") && !text.contains("usize"),
        "{text}"
    );
    assert_eq!(out.trim_end(), "[[0, 3], [1], [2]] 1 4", "{text}");
}

#[test]
fn kata_1057_usize_alone_draws_nothing() {
    let src = "fn main() {\n    let n: usize = 3usize;\n    println(n);\n}";
    let (first, text, clean, out) = mend(&scratch("usize-alone"), src);
    assert!(!first.contains("FFI signatures"), "{first}");
    assert!(clean && text.contains("let n: usize"), "{text}");
    assert_eq!(out.trim_end(), "3");
}

#[test]
fn kata_1057_reverse_around_heap_items_is_dropped() {
    let src = "use std::cmp::Reverse;\nuse std::collections::BinaryHeap;\nfn main() {\n    let start: Vec<(i64, i64)> = Vec::new();\n    let mut heap = BinaryHeap::from(start);\n    for (d, i) in [(5, 0), (2, 1), (9, 2), (2, 0)] {\n        heap.push(Reverse((d, i)));\n    }\n    let mut order = Vec::new();\n    while let Some(Reverse((d, i))) = heap.pop() {\n        order.push(d * 10 + i);\n    }\n    println(f\"{order}\");\n}";
    let (first, text, clean, out) = mend(&scratch("reverse-heap"), src);
    assert!(
        first.contains("pops the smallest first already"),
        "no Reverse hint on the first check:\n{first}"
    );
    assert!(!first.contains("Release"), "{first}");
    assert!(
        first.contains("no need of `Reverse` here") && !first.contains("`Reverse` in its prelude"),
        "{first}"
    );
    assert!(clean, "still refused after fix:\n{text}");
    assert!(
        text.contains("heap.push((d, i))")
            && text.contains("Some((d, i))")
            && text.contains("PriorityQueue.from(start)")
            && !text.contains("Reverse")
            && !text.contains("max_first"),
        "{text}"
    );
    assert_eq!(out.trim_end(), "[20, 21, 50, 92]", "{text}");
}

#[test]
fn kata_1057_reverse_sort_key_takes_the_descending_sort_by() {
    // Was `kata_1057_reverse_in_a_sort_key_has_no_fix`: kata 1058 gave the
    // whole-body sort key its fix.
    let src = "use std::cmp::Reverse;\nfn main() {\n    let mut v = [3, 1, 2];\n    v.sort_by_key(|x| Reverse(*x));\n    println(f\"{v}\");\n}";
    let (first, text, clean, out) = mend(&scratch("reverse-sort"), src);
    assert!(!first.contains("Release"), "{first}");
    assert!(clean, "still refused after fix:\n{text}");
    assert!(
        text.contains("v.sort_by(|a, b| b.cmp(a));") && !text.contains("Reverse"),
        "{text}"
    );
    assert_eq!(out.trim_end(), "[3, 2, 1]", "{text}");
}

#[test]
fn kata_1057_max_first_heap_without_reverse_is_unchanged() {
    let src = "use std::collections::BinaryHeap;\nfn main() {\n    let mut heap = BinaryHeap::from([4, 1]);\n    heap.push(7);\n    println(heap.pop().unwrap());\n}";
    let (_, text, clean, out) = mend(&scratch("heap-max"), src);
    assert!(
        clean && text.contains("PriorityQueue.max_first_from([4, 1])"),
        "{text}"
    );
    assert_eq!(out.trim_end(), "7");
}
