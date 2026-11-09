//! Kata 1086, High Five: LeetCode's Rust template keeps five scores per
//! student in a `BTreeMap<i32, BinaryHeap<Reverse<i32>>>`, builds each heap
//! with `entry(id).or_insert_with(BinaryHeap::new)`, and sums it with
//! `heap.into_iter().map(|Reverse(s)| s).sum()`. Three gaps stood between
//! that and a clean check:
//!
//! - `PriorityQueue` had no `iter`, `into_iter`, `for`, `Clone` or
//!   `Display`, though collections.md says every collection works with
//!   `for` and is `Display` (B-2026-11-09-41).
//! - `|Reverse(s)|` as a closure parameter got only "a parameter's pattern
//!   must be irrefutable", with no fix, and `BinaryHeap::new` named as a
//!   value was not seen as a heap's constructor, so neither it nor any
//!   `Reverse` in the program got the smallest-first fix (B-2026-11-09-42).

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

/// `karac fix` run until it changes nothing (six rounds at most): the
/// first check's diagnostics, the text left, whether it checks clean, and
/// what it prints under the MIR interpreter.
fn fixed(name: &str, src: &str) -> (String, String, bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-1086-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), format!("{src}\n")).unwrap();
    let (_, out, err) = karac(&dir, &["check", "cell.kara"]);
    let first = format!("{out}{err}");
    let mut text = std::fs::read_to_string(dir.join("cell.kara")).unwrap();
    for _ in 0..6 {
        karac(&dir, &["fix", "cell.kara"]);
        let next = std::fs::read_to_string(dir.join("cell.kara")).unwrap();
        if next == text {
            break;
        }
        text = next;
    }
    let (clean, _, _) = karac(&dir, &["check", "cell.kara"]);
    let (_, out, _) = karac(&dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (first, text, clean, out)
}

#[test]
fn kata_1086_a_priority_queue_iterates_clones_and_prints() {
    let src = r#"fn total[C: Iterable[Item = ref i64]](c: C) -> i64 {
    let mut t = 0;
    for x in c.iter() {
        t += x;
    }
    t
}

fn main() {
    let mut pq: PriorityQueue[i64] = PriorityQueue.max_first();
    for x in [5, 1, 4, 9, 2].iter() {
        pq.push(x);
    }
    let mut s = 0;
    for x in pq {
        s += x;
    }
    let t: i64 = pq.iter().sum();
    println(f"{pq} {s} {t} {total(pq)}");
    let q = pq.clone();
    pq.pop();
    println(f"{pq} {q}");
    let v: Vec[i64] = q.into_iter().collect();
    println(f"{v}");
    let mut ps: PriorityQueue[String] = PriorityQueue.new();
    ps.push("b".to_string());
    ps.push("a".to_string());
    let c = ps.clone();
    let w: Vec[String] = ps.into_iter().map(|s| s + "!").collect();
    println(f"{w} {c.into_vec()}");
}"#;
    let (first, text, clean, out) = fixed("iter", src);
    assert!(first.contains("All checks passed"), "{first}");
    assert!(clean, "{text}");
    assert_eq!(
        out.trim_end(),
        "[9, 5, 4, 1, 2] 21 21 21\n[5, 2, 4, 1] [9, 5, 4, 1, 2]\n[9, 5, 4, 1, 2]\n[a!, b!] [a, b]"
    );
}

#[test]
fn kata_1086_the_reverse_heap_template_is_fixed_to_a_clean_check() {
    let src = r#"use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};

fn main() {
    let items: Vec<(i32, i32)> = vec![(1, 5), (1, 9), (2, 3), (1, 2), (2, 8), (1, 7)];
    let mut map: BTreeMap<i32, BinaryHeap<Reverse<i32>>> = BTreeMap::new();
    for &(id, s) in items.iter() {
        let heap = map.entry(id).or_insert_with(BinaryHeap::new);
        heap.push(Reverse(s));
        if heap.len() > 2 {
            heap.pop();
        }
    }
    for (id, heap) in map.into_iter() {
        let sum: i32 = heap.into_iter().map(|Reverse(s)| s).sum();
        println!("{} {}", id, sum);
    }
}"#;
    let (first, text, clean, out) = fixed("template", src);
    assert!(
        first.contains("`Reverse(s)` is Rust's `std::cmp::Reverse`"),
        "{first}"
    );
    assert!(clean, "still refused:\n{text}");
    assert!(text.contains("or_insert_with(PriorityQueue.new)"), "{text}");
    assert!(text.contains("|s| s"), "{text}");
    assert!(!text.contains("Reverse"), "{text}");
    // The two largest of each student's scores: 9 + 7, and 3 + 8.
    assert_eq!(out.trim_end(), "1 16\n2 11", "{text}");
}

#[test]
fn kata_1086_a_refutable_parameter_of_another_reverse_keeps_its_error() {
    let src = r#"enum Way {
    Reverse(i64),
    Ahead(i64),
}

fn main() {
    let ways = [Way.Ahead(1)];
    let n: Vec[i64] = ways.iter().map(|Reverse(s)| s).collect();
    println(f"{n}");
}"#;
    let (first, text, clean, _) = fixed("enum", src);
    assert!(
        first.contains("a parameter's pattern must be irrefutable, and `Reverse(..)`"),
        "{first}"
    );
    assert!(!first.contains("std::cmp::Reverse"), "{first}");
    assert!(!clean, "{text}");
}
