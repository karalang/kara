//! B-2026-11-24-28 (kata 2462): LeetCode's Rust template keeps two heaps of
//! `Reverse((cost, index))`, named `front` and `back`. Which way a Rust
//! heap pops is read from the tokens around it, and `karac check` reads
//! the program with the Kāra library appended, whose `collections.kara`
//! pushes bare items with `self.front.push(x)` and `self.back.push(x)`.
//! That was taken as evidence about the program's heaps of the same
//! names, so each was "cannot be told", and `karac fix` stopped with both
//! `BinaryHeap`s unfixed and an `import std.cmp.Reverse;` written. The
//! same template with the heaps named anything else was carried.

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-k2462-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    f
}

fn karac(args: &[&str], f: &PathBuf) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(f)
        .output()
        .expect("karac");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

const TEMPLATE: &str = "use std::collections::BinaryHeap;
use std::cmp::Reverse;

struct Solution;

impl Solution {
    pub fn total_cost(costs: Vec<i32>, k: i32, candidates: i32) -> i64 {
        let n = costs.len();
        let c = candidates as usize;
        let mut front = BinaryHeap::new();
        let mut back = BinaryHeap::new();
        let mut i = 0;
        let mut j = n;
        while i < c && i < j {
            front.push(Reverse((costs[i], i)));
            i += 1;
        }
        while j > i && n - j < c {
            j -= 1;
            back.push(Reverse((costs[j], j)));
        }
        let mut total: i64 = 0;
        for _ in 0..k {
            let take_front = match (front.peek(), back.peek()) {
                (Some(&Reverse(a)), Some(&Reverse(b))) => a <= b,
                (Some(_), None) => true,
                _ => false,
            };
            if take_front {
                let Reverse((cost, _)) = front.pop().unwrap();
                total += cost as i64;
                if i < j {
                    front.push(Reverse((costs[i], i)));
                    i += 1;
                }
            } else {
                let Reverse((cost, _)) = back.pop().unwrap();
                total += cost as i64;
                if i < j {
                    j -= 1;
                    back.push(Reverse((costs[j], j)));
                }
            }
        }
        total
    }
}

fn main() {
    println(Solution.total_cost([17i32, 12, 10, 2, 7, 2, 11, 20, 8].to_vec(), 3, 4));
    println(Solution.total_cost([1i32, 2, 4, 1].to_vec(), 3, 3));
}
";

#[test]
fn heaps_named_front_and_back_are_carried_to_priority_queues() {
    let f = program("template", TEMPLATE);
    let (ok, out) = karac(&["fix"], &f);
    assert!(ok, "fix: {out}");
    let fixed = std::fs::read_to_string(&f).unwrap();
    assert!(
        fixed.contains("let mut front = PriorityQueue.new();"),
        "{fixed}"
    );
    assert!(
        fixed.contains("let mut back = PriorityQueue.new();"),
        "{fixed}"
    );
    assert!(!fixed.contains("Reverse"), "{fixed}");
    let (ok, out) = karac(&["check"], &f);
    assert!(ok, "check after fix: {out}");
    let (ok, out) = karac(&["run", "--interp"], &f);
    assert!(ok, "run: {out}");
    assert_eq!(out, "11\n4\n");
}

#[test]
fn a_heap_named_back_pushed_reversed_is_smallest_first() {
    let f = program(
        "back",
        "fn main() {
    let mut back = BinaryHeap.new();
    back.push(Reverse(3));
    println(back.len());
}
",
    );
    let (_, out) = karac(&["check"], &f);
    assert!(!out.contains("cannot be told"), "{out}");
    assert!(
        out.contains("so `BinaryHeap.new()` is `PriorityQueue.new()`"),
        "{out}"
    );
}
