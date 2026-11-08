//! Rust spellings kata 2008's arms met, each carried by `karac fix` to Kāra.
//!
//! - `BinaryHeap[Reverse[T]]`, the heap's item type: kata 1057's fix drops
//!   `Reverse(..)` around the items pushed and popped, but the type took the
//!   did-you-mean `Release`. The type's `Reverse[..]` now goes too.
//! - Rust's `mut v: T` parameter, owned and changed by the body: the
//!   diagnostic had no edit. It now writes `v: own T` and `let mut v = v;`.
//! - `let (end, tip) = (r[1] as usize, r[2] as i64);` over `i32`s, where
//!   `end` then meets an `i64`: only a single-name `let` had the fix that
//!   writes the cast `as i64`. Each cast a tuple `let` binds by name has it.
//! - `m.range(a..=b)` on a `SortedMap` or `SortedSet`, Rust's one range
//!   argument: the arity error had no edit. It now writes
//!   `range_inclusive(a, b)`, or `range(a, b)` for `a..b`.

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
    std::env::temp_dir().join(format!("karac-kata-2008-{name}-{}", std::process::id()))
}

#[test]
fn kata_2008_heap_reverse_item_type_is_dropped() {
    let src = "fn main() {\n    let mut q: BinaryHeap[Reverse[(i64, i64)]] = BinaryHeap.new();\n    q.push(Reverse((3, 1)));\n    q.push(Reverse((1, 2)));\n    if let Some(Reverse((a, b))) = q.pop() {\n        println(f\"{a} {b}\");\n    }\n}";
    let (first, text, clean, out) = mend(&scratch("heap_reverse_item_ty"), src);
    assert!(!first.contains("Release"), "{first}");
    assert!(
        text.contains("let mut q: PriorityQueue[(i64, i64)] = PriorityQueue.new();"),
        "{text}"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "1 2\n");
}

#[test]
fn kata_2008_rust_mut_parameter_becomes_own_with_let_mut() {
    let src = "fn sorted(mut v: Vec[i64]) -> Vec[i64] {\n    v.sort();\n    v\n}\n\nfn main() {\n    let xs = [3, 1, 2];\n    println(f\"{sorted(xs.clone())} {xs}\");\n}";
    let (first, text, clean, out) = mend(&scratch("rust_mut_parameter_b"), src);
    assert!(first.contains("`v: own T` and `let mut v = v;`"), "{first}");
    assert!(
        text.contains(
            "fn sorted(v: own Vec[i64]) -> Vec[i64] {\n    let mut v = v;\n    v.sort();"
        ),
        "{text}"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "[1, 2, 3] [3, 1, 2]\n");
}

#[test]
fn kata_2008_tuple_let_of_usize_casts_becomes_i64() {
    let src = "fn gain(r: Vec[i32], i: i64) -> i64 {\n    let (end, tip) = (r[1] as usize, r[2] as i64);\n    let reach: i64 = end;\n    reach - i + tip\n}\n\nfn main() {\n    println(gain([1, 5, 2], 1));\n}";
    let (first, text, clean, out) = mend(&scratch("tuple_let_of_usize_c"), src);
    assert!(
        text.contains("let (end, tip) = (r[1] as i64, r[2] as i64);"),
        "{text}"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "6\n");
}

#[test]
fn kata_2008_sorted_map_range_with_one_range_argument() {
    let src = "fn main() {\n    let mut best: SortedMap[i64, i64] = SortedMap.new();\n    best.insert(1, 10);\n    best.insert(3, 30);\n    best.insert(5, 50);\n    let a: Vec[(i64, i64)] = best.range(1..=3).collect();\n    let b: Vec[(i64, i64)] = best.range(1..3).collect();\n    let s: SortedSet[i64] = [4, 6, 8].iter().collect();\n    let c: Vec[i64] = s.range(4..=6).collect();\n    println(f\"{a} {b} {c}\");\n}";
    let (first, text, clean, out) = mend(&scratch("sorted_map_range_wit"), src);
    assert!(first.contains("`range_inclusive(1, 3)`"), "{first}");
    assert!(
        text.contains("best.range_inclusive(1, 3)") && text.contains("best.range(1, 3)"),
        "{text}"
    );
    assert!(text.contains("s.range_inclusive(4, 6)"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "[(1, 10), (3, 30)] [(1, 10)] [4, 6]\n");
}
