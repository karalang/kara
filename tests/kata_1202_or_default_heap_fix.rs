//! B-2026-11-18-46: Rust's `heaps.entry(r).or_default()` on a
//! `HashMap<usize, BinaryHeap<Reverse<char>>>` (kata 1202's heap arm) was
//! fixed to `or_insert(PriorityQueue.new())`, Kāra's smallest-first queue,
//! where every other `BinaryHeap` constructor becomes the largest-first
//! `PriorityQueue.max_first()`: the fixed program checked, ran, and popped
//! the wrong end. The fix now takes the heap Rust's was, read from the map's
//! declaration while the source still says `BinaryHeap`: `max_first()`, or
//! `new()` for a heap of `Reverse` items where the program drops `Reverse`.
//! A map of Kāra's own `PriorityQueue` keeps `new()`.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-or-default-heap-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

/// `karac fix` on `src`, then the fixed text and what it prints.
fn fix_and_run(tag: &str, src: &str) -> (String, String) {
    let (dir, path) = fixture(tag, src);
    karac().arg("fix").arg(&path).output().unwrap();
    let fixed = std::fs::read_to_string(&path).unwrap();
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert!(
        run.status.success(),
        "{fixed}\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    (fixed, String::from_utf8_lossy(&run.stdout).into_owned())
}

#[test]
fn a_rust_map_of_heaps_or_default_is_fixed_to_the_largest_first_queue() {
    let src = "use std::collections::{BinaryHeap, HashMap};\n\nfn main() {\n    let mut m: HashMap<i64, BinaryHeap<i64>> = HashMap::new();\n    m.entry(0).or_default().push(1);\n    m.entry(0).or_default().push(5);\n    println!(\"{}\", m.get_mut(&0).unwrap().pop().unwrap());\n}\n";
    let (fixed, out) = fix_and_run("plain", src);
    assert_eq!(fixed, "fn main() {\n    let mut m: Map[i64, PriorityQueue[i64]] = Map.new();\n    m.entry(0).or_insert(PriorityQueue.max_first()).push(1);\n    m.entry(0).or_insert(PriorityQueue.max_first()).push(5);\n    println(f\"{m.get_mut(0).unwrap().pop().unwrap()}\");\n}\n");
    assert_eq!(out, "5\n");
}

#[test]
fn a_rust_map_of_heaps_in_a_field_or_a_parameter_takes_the_largest_first_queue() {
    let src = "use std::cmp::Reverse;\nuse std::collections::{BinaryHeap, HashMap};\n\nstruct Book {\n    heaps: HashMap<i64, BinaryHeap<i64>>,\n}\n\nfn add(m: &mut HashMap<i64, BinaryHeap<Reverse<i64>>>, k: i64, v: i64) {\n    m.entry(k).or_default().push(Reverse(v));\n}\n\nfn main() {\n    let mut b = Book { heaps: HashMap::new() };\n    b.heaps.entry(1).or_default().push(3);\n    b.heaps.entry(1).or_default().push(9);\n    let mut m: HashMap<i64, BinaryHeap<Reverse<i64>>> = HashMap::new();\n    add(&mut m, 0, 4);\n    add(&mut m, 0, 2);\n    let Reverse(y) = m.get_mut(&0).unwrap().pop().unwrap();\n    println!(\"{} {}\", b.heaps.get_mut(&1).unwrap().pop().unwrap(), y);\n}\n";
    let (fixed, out) = fix_and_run("places", src);
    assert_eq!(fixed, "import std.cmp.Reverse;\n\nstruct Book {\n    heaps: Map[i64, PriorityQueue[i64]],\n}\n\nfn add(m: mut ref Map[i64, PriorityQueue[Reverse[i64]]], k: i64, v: i64) {\n    m.entry(k).or_insert(PriorityQueue.max_first()).push(Reverse { value: v });\n}\n\nfn main() {\n    let mut b = Book { heaps: Map.new() };\n    b.heaps.entry(1).or_insert(PriorityQueue.max_first()).push(3);\n    b.heaps.entry(1).or_insert(PriorityQueue.max_first()).push(9);\n    let mut m: Map[i64, PriorityQueue[Reverse[i64]]] = Map.new();\n    add(mut m, 0, 4);\n    add(mut m, 0, 2);\n    let Reverse { value: y } = m.get_mut(0).unwrap().pop().unwrap();\n    println(f\"{b.heaps.get_mut(1).unwrap().pop().unwrap()} {y}\");\n}\n");
    assert_eq!(out, "9 2\n");
}

#[test]
fn a_rust_map_of_reverse_heaps_whose_reverse_is_dropped_takes_the_smallest_first_queue() {
    let src = "use std::cmp::Reverse;\nuse std::collections::{BinaryHeap, HashMap};\n\nfn main() {\n    let mut h: BinaryHeap<Reverse<i64>> = BinaryHeap::new();\n    h.push(Reverse(7));\n    h.push(Reverse(2));\n    let Reverse(x) = h.pop().unwrap();\n    let mut m: HashMap<i64, BinaryHeap<Reverse<i64>>> = HashMap::new();\n    m.entry(0).or_default().push(Reverse(1));\n    m.entry(0).or_default().push(Reverse(5));\n    let Reverse(y) = m.get_mut(&0).unwrap().pop().unwrap();\n    println!(\"{} {}\", x, y);\n}\n";
    let (fixed, out) = fix_and_run("dropped", src);
    assert_eq!(fixed, "fn main() {\n    let mut h: PriorityQueue[i64] = PriorityQueue.new();\n    h.push(7);\n    h.push(2);\n    let x = h.pop().unwrap();\n    let mut m: Map[i64, PriorityQueue[i64]] = Map.new();\n    m.entry(0).or_insert(PriorityQueue.new()).push(1);\n    m.entry(0).or_insert(PriorityQueue.new()).push(5);\n    let y = m.get_mut(0).unwrap().pop().unwrap();\n    println(f\"{x} {y}\");\n}\n");
    assert_eq!(out, "2 1\n");
}

#[test]
fn a_kara_map_of_priority_queues_or_default_keeps_the_plain_queue() {
    let src = "fn main() {\n    let mut m: Map[i64, PriorityQueue[i64]] = Map.new();\n    m.entry(0).or_default().push(5);\n    m.entry(0).or_default().push(1);\n    println(f\"{m.get_mut(0).unwrap().pop().unwrap()}\");\n}\n";
    let (fixed, out) = fix_and_run("kara", src);
    assert_eq!(
        fixed,
        src.replace("or_default()", "or_insert(PriorityQueue.new())")
    );
    assert_eq!(out, "1\n");
}
