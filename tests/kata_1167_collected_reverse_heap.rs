//! Kata 1167's Rust arm built its heap the other common way, `let mut heap:
//! BinaryHeap<Reverse<i32>> = sticks.into_iter().map(Reverse).collect();`,
//! and `karac fix` renamed the type and kept `Reverse`, so Kāra's
//! smallest-first `PriorityQueue` popped the largest stick first: 16 for
//! `[2,4,3]`, not 14 (B-2026-11-15-86). A collected heap of `Reverse` items now
//! goes to `PriorityQueue.max_first_from(..)` as B-2026-11-15-80 sends a
//! plain one, and `.map(Reverse)`, Rust's tuple-struct constructor passed
//! as a function, is fixed to `.map(|x| Reverse { value: x })`.

use std::path::Path;
use std::process::{Command, Stdio};

/// Runs `karac` on `src` as `cell.kara`: its verdict, its output, and the
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

/// `karac fix` until nothing is left to fix, then `check` and `__mir-run`:
/// the fixed source and what it prints.
fn mend(tag: &str, src: &str) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-1167-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let mut fixed = String::new();
    for _ in 0..8 {
        let (_, _, now) = karac(&dir, &["fix"]);
        if now == fixed {
            break;
        }
        fixed = now;
    }
    let (ok, out, _) = karac(&dir, &["check"]);
    assert!(ok, "{tag}: {out}\n{fixed}");
    let (ok, out, _) = karac(&dir, &["__mir-run"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{tag}: {out}\n{fixed}");
    (fixed, out)
}

const COLLECTED: &str = r#"use std::collections::BinaryHeap;
use std::cmp::Reverse;
struct Solution {}
impl Solution {
    pub fn connect_sticks(sticks: Vec<i32>) -> i32 {
        let mut heap: BinaryHeap<Reverse<i32>> = sticks.into_iter().map(Reverse).collect();
        let mut ans = 0;
        while heap.len() > 1 {
            let Reverse(a) = heap.pop().unwrap();
            let Reverse(b) = heap.pop().unwrap();
            ans += a + b;
            heap.push(Reverse(a + b));
        }
        ans
    }
}
fn main() {
    println(f"{Solution.connect_sticks([2, 4, 3])} {Solution.connect_sticks([1, 8, 3, 5])}");
}
"#;

const BUILT: &str = r#"use std::collections::BinaryHeap;
use std::cmp::Reverse;
fn cost(sticks: Vec<i32>) -> i32 {
    let mut heap = BinaryHeap::new();
    for s in sticks {
        heap.push(Reverse(s));
    }
    let mut ans = 0;
    while heap.len() > 1 {
        let Reverse(a) = heap.pop().unwrap();
        let Reverse(b) = heap.pop().unwrap();
        ans += a + b;
        heap.push(Reverse(a + b));
    }
    ans
}
fn main() {
    println(f"{cost([2, 4, 3])} {cost([1, 8, 3, 5])}");
}
"#;

#[test]
fn kata_1167_a_collected_reverse_heap_pops_the_smallest_first() {
    let (fixed, out) = mend("collected", COLLECTED);
    assert_eq!(out.trim(), "14 30", "{fixed}");
    assert!(fixed.contains("PriorityQueue.max_first_from("), "{fixed}");
    assert!(fixed.contains(".map(|x| Reverse { value: x })"), "{fixed}");
}

#[test]
fn kata_1167_a_reverse_heap_built_by_push_still_drops_reverse() {
    let (fixed, out) = mend("built", BUILT);
    assert_eq!(out.trim(), "14 30", "{fixed}");
    assert!(!fixed.contains("max_first"), "{fixed}");
}
