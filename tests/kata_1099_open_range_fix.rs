//! Kata 1099's one-pass arm keeps the values seen so far in a
//! `SortedSet[i64]` and takes the best partner of each value as Rust
//! writes it, `seen.range(..k - x).next_back()`. Kāra's `range` takes both
//! ends, so the call is refused, and since kata 2070 the refusal names the
//! end to supply; `karac fix` had nothing to apply. Over an integer
//! element (or key) type `T`, the missing end is `T.MIN` or `T.MAX`, so
//! the edit is now written: `..b` to `range(T.MIN, b)`, `..=b` to
//! `range_inclusive(T.MIN, b)`, `a..` to `range_inclusive(a, T.MAX)`, and
//! `..` to `iter()` over any type. Over any other type the advice stands
//! alone (B-2026-11-10-31).

use std::process::{Command, Stdio};

/// `karac fix` rounds until nothing changes.
fn fixed_fully(src: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "karac-kata1099-{}-{}.kara",
        std::process::id(),
        src.len()
    ));
    std::fs::write(&path, src).unwrap();
    for _ in 0..6 {
        let before = std::fs::read_to_string(&path).unwrap();
        let _ = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(["fix", path.to_str().unwrap()])
            .stdin(Stdio::null())
            .output()
            .expect("karac");
        if std::fs::read_to_string(&path).unwrap() == before {
            break;
        }
    }
    let out = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    out
}

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .unwrap_or_else(|e| panic!("build: {e}\n{src}"))
        .output
}

#[test]
fn kata_1099_open_start_on_an_i64_sorted_set_takes_i64_min() {
    let src = "fn two_sum_less_than_k(nums: Vec[i64], k: i64) -> i64 {
    let mut seen: SortedSet[i64] = SortedSet.new();
    let mut best = -1;
    for x in nums.iter() {
        if let Some(y) = seen.range(..k - x).next_back() {
            best = best.max(x + y);
        }
        seen.insert(x);
    }
    best
}

fn main() {
    let a = two_sum_less_than_k([34, 23, 1, 24, 75, 33, 54, 8], 60);
    let b = two_sum_less_than_k([10, 20, 30], 15);
    println(f\"{a} {b}\");
}
";
    assert!(
        karac::mir::lower::run_source(src).is_err(),
        "refused before the fix"
    );
    let out = fixed_fully(src);
    assert!(
        out.contains("seen.range(i64.MIN, k - x).next_back()"),
        "{out}"
    );
    assert_eq!(run(&out), "58 -1\n");
}

#[test]
fn kata_1099_inclusive_and_open_end_ranges_take_the_key_type_bounds() {
    let src = "fn main() {
    let mut m: SortedMap[i32, i32] = SortedMap.new();
    for k in [1i32, 3, 5, 7, 9] {
        m.insert(k, k * 10);
    }
    let a = m.range(..=5).count();
    let b = m.range(3..).count();
    let c = m.range(..).count();
    let mut s: SortedSet[u8] = SortedSet.new();
    s.insert(3u8);
    s.insert(255u8);
    let d = s.range(200u8..).count();
    println(f\"{a} {b} {c} {d}\");
}
";
    let out = fixed_fully(src);
    assert!(out.contains("m.range_inclusive(i32.MIN, 5)"), "{out}");
    assert!(out.contains("m.range_inclusive(3, i32.MAX)"), "{out}");
    assert!(out.contains("m.iter().count()"), "{out}");
    assert!(out.contains("s.range_inclusive(200u8, u8.MAX)"), "{out}");
    assert_eq!(run(&out), "3 4 5 1\n");
}

#[test]
fn kata_1099_open_range_over_strings_keeps_the_advice_only() {
    let src = "fn main() {
    let mut s: SortedSet[String] = SortedSet.new();
    s.insert(\"ant\".to_string());
    s.insert(\"bee\".to_string());
    let a = s.range(..\"b\".to_string()).count();
    println(f\"{a}\");
}
";
    let out = fixed_fully(src);
    assert_eq!(out, src, "no edit over a non-integer element");
}
