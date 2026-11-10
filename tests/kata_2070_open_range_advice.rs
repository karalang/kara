//! Kata 2070's Rust arm finds the best beauty at a price at most `q` with
//! Rust's `best.range(..=q).next_back()` on a `SortedMap`. Kāra's `range`
//! and `range_inclusive` take both ends, so the call is refused, and a
//! closed `a..=b` comes with the edit to `range_inclusive(a, b)`; a range
//! open at an end came with nothing past "method 'range' expects 2
//! argument(s), found 1". It now names the end to supply, and on a
//! `SortedMap` the lookup the open range stands in for: `floor(q)` for
//! `..=q`, `ceiling(a)` for `a..`.

use std::process::{Command, Stdio};

fn check(name: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "kata_2070_open_range_{name}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", file.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn kata_2070_open_range_on_a_sorted_map_names_floor_and_ceiling() {
    let out = check(
        "map",
        "fn main() {
    let mut m: SortedMap[i64, i64] = SortedMap.new();
    m.insert(3, 30);
    let a = m.range(..=4).next_back();
    let b = m.range(2..).next();
    println(f\"{a} {b}\");
}
",
    );
    assert!(
        out.contains("Rust's range has no start, so name the smallest key wanted: `range_inclusive(from, to)`; the entry with the largest key at most `to`, Rust's `range(..=to).next_back()`, is `floor(to)`"),
        "{out}"
    );
    assert!(
        out.contains("Rust's range has no end, so name the largest key wanted: `range_inclusive(from, to)`; the entry with the smallest key at least `from`, Rust's `range(from..).next()`, is `ceiling(from)`"),
        "{out}"
    );
}

#[test]
fn kata_2070_open_range_on_a_sorted_set_names_the_missing_end() {
    let out = check(
        "set",
        "fn main() {
    let s: SortedSet[i64] = [1, 4, 9].iter().collect();
    let a: Vec[i64] = s.range(..4).collect();
    let b: Vec[i64] = s.range(..).collect();
    println(f\"{a} {b}\");
}
",
    );
    assert!(
        out.contains("Rust's range has no start, so name the smallest element wanted: `range(from, to)` (docs/library/collections.md)"),
        "{out}"
    );
    assert!(
        out.contains("Rust's `range(..)` is the whole SortedSet in order, `iter()`"),
        "{out}"
    );
    assert!(!out.contains("floor"), "{out}");
}
