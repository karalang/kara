//! Kata 1172's Rust arm keeps the stacks with room in a
//! `BinaryHeap<Reverse<usize>>` and reads its top with `while let
//! Some(&Reverse(i)) = self.available.peek()`. The `&` before `Reverse`
//! made the parser keep every `Reverse` (as `std.cmp.Reverse`), so the first
//! `karac fix` round wrote `import std.cmp.Reverse;`; the next round, with
//! the `&` gone, read the heap as smallest-first and built it with
//! `PriorityQueue.new()`. A smallest-first queue of `Reverse` values pops the
//! LARGEST index, the opposite of Rust's, and nothing reported it. Now
//! `Some(&Reverse(i))` is a heap item like `Some(Reverse(i))`, so the
//! `Reverse` goes in the first round, and once a program imports Kāra's
//! `std.cmp.Reverse` its heaps are read as keeping it: largest-first
//! `PriorityQueue.max_first()` over `Reverse` values. B-2026-11-16-185.

use std::path::Path;
use std::process::{Command, Stdio};

/// Runs `karac` on `cell.kara` in `dir`: its verdict, its output, and the
/// file as it stands after.
fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("cell.kara")
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let fixed = std::fs::read_to_string(dir.join("cell.kara")).unwrap_or_default();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        fixed,
    )
}

fn cell(tag: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-kata-1172-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    dir
}

/// One `karac fix` round over `src`, then a second that must find nothing,
/// then `__mir-run`: the source after the one round, and what it prints.
fn one_round(tag: &str, src: &str) -> (String, String) {
    let dir = cell(tag, src);
    let (_, out, fixed) = karac(&dir, &["fix"]);
    assert!(out.contains("applied"), "{tag}: {out}");
    let (_, out, again) = karac(&dir, &["fix"]);
    assert_eq!(
        again, fixed,
        "{tag}: a second round changed the source: {out}"
    );
    let (ok, out, _) = karac(&dir, &["check"]);
    assert!(ok, "{tag}: {out}\n{fixed}");
    let (ok, out, _) = karac(&dir, &["__mir-run"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{tag}: {out}\n{fixed}");
    (fixed, out)
}

#[test]
fn kata_1172_peeked_reverse_heap_drops_reverse_and_pops_smallest() {
    let (fixed, out) = one_round(
        "peek",
        r#"use std::collections::BinaryHeap;
use std::cmp::Reverse;

struct Q {
    heap: BinaryHeap<Reverse<usize>>,
}

impl Q {
    fn new() -> Self {
        Q { heap: BinaryHeap::new() }
    }

    fn low(&mut self) -> usize {
        while let Some(&Reverse(i)) = self.heap.peek() {
            if i > 1 {
                break;
            }
            self.heap.pop();
        }
        if let Some(&Reverse(i)) = self.heap.peek() { i } else { 0 }
    }
}

fn main() {
    let mut q = Q::new();
    q.heap.push(Reverse(1));
    q.heap.push(Reverse(3));
    q.heap.push(Reverse(2));
    println!("{}", q.low());
}
"#,
    );
    assert!(fixed.contains("heap: PriorityQueue[usize],"), "{fixed}");
    assert!(fixed.contains("Q { heap: PriorityQueue.new() }"), "{fixed}");
    assert!(
        fixed.contains("while let Some(i) = self.heap.peek().cloned() {"),
        "{fixed}"
    );
    assert!(!fixed.contains("Reverse"), "{fixed}");
    assert_eq!(out.trim(), "2");
}

#[test]
fn kata_1172_kept_std_cmp_reverse_heap_is_largest_first() {
    let (fixed, out) = one_round(
        "kept",
        r#"import std.cmp.Reverse;

struct Q {
    heap: BinaryHeap[Reverse[usize]],
}

impl Q {
    fn new() -> Self {
        Q { heap: BinaryHeap.new() }
    }

    fn low(mut ref self) -> usize {
        while let Some(Reverse(i)) = self.heap.peek().cloned() {
            if i > 1 {
                break;
            }
            self.heap.pop();
        }
        if let Some(Reverse(i)) = self.heap.peek().cloned() { i } else { 0 }
    }
}

fn main() {
    let mut q = Q.new();
    q.heap.push(Reverse(1));
    q.heap.push(Reverse(3));
    q.heap.push(Reverse(2));
    println(f"{q.low()}");
}
"#,
    );
    assert!(
        fixed.contains("heap: PriorityQueue[Reverse[usize]],"),
        "{fixed}"
    );
    assert!(
        fixed.contains("Q { heap: PriorityQueue.max_first() }"),
        "{fixed}"
    );
    assert_eq!(out.trim(), "2");
}
