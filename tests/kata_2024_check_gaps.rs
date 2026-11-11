//! Kata 2024's Rust arm: LeetCode's template keeps its window's left end
//! as `let (mut l, mut cnt, mut res) = (0usize, 0, 0)`, and the end meets
//! the `i64` of `0..s.len()` only in arithmetic, `r - l + 1`.
//!
//! - The `usize` suffix had its fix to `i64` only when an assignment or a
//!   comparison mixed the two; an arithmetic mix with no `as usize` to drop
//!   was refused with no fix at all.
//! - `karac fix` drops the closure's `-> i32`, which was what typed `res`;
//!   the literal then took the default `i64`, and `res.max(x as i32)` was
//!   refused with no fix and no binding named. A tuple element's or a
//!   fill's literal is inline, so the fix is its suffix, `0i32`
//!   (design.md §8 Literal suffixes; B-2026-11-08-53). Katas 2006
//!   (`[0; 101]`) and 2012 (`Vec.filled(n, 0)` under `max`) are the fill
//!   shapes.

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
    std::env::temp_dir().join(format!("karac-kata-2024-{name}-{}", std::process::id()))
}

#[test]
fn kata_2024_usize_suffix_mixed_only_in_arithmetic_gets_its_fix() {
    let src = "fn longest(v: Vec[i64]) -> i64 {\n    let mut (l, best) = (0usize, 0);\n    for r in 0..v.len() {\n        if v[l] < 0 {\n            l += 1;\n        }\n        best = best.max(r - l + 1);\n    }\n    best\n}\n\nfn main() {\n    println(longest([-1, 2, 3]));\n}";
    let (first, text, clean, out) = mend(&scratch("usize_suffix"), src);
    assert!(first.contains("drop the suffix"), "{first}");
    assert!(text.contains("let mut (l, best) = (0, 0);"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "2\n");
}

#[test]
fn kata_2024_tuple_literal_under_max_gets_its_suffix() {
    let src = "fn longest(v: Vec[i32], k: i32) -> i32 {\n    let mut (l, cnt, res) = (0, 0, 0);\n    for r in 0..v.len() {\n        if v[r] < 0 {\n            cnt += 1;\n        }\n        while cnt > k {\n            if v[l] < 0 {\n                cnt -= 1;\n            }\n            l += 1;\n        }\n        res = res.max((r - l + 1) as i32);\n    }\n    res\n}\n\nfn main() {\n    println(longest([1, -1, 2, -1, -1, 3], 1));\n}";
    let (first, text, clean, out) = mend(&scratch("tuple_max"), src);
    assert!(
        first.contains("`res` is an 'i64' because the literal it is bound to took the default"),
        "{first}"
    );
    assert!(
        text.contains("let mut (l, cnt, res) = (0, 0, 0i32);"),
        "{text}"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "3\n");
}

#[test]
fn kata_2024_array_fill_literal_narrowed_gets_its_suffix() {
    // `ans += cnt[x]` into an `i32` is an assignment to `ans`, so it fixes
    // the fill's open literal as `i32` (design.md §8 Literals, the kata 2103
    // ruling) and the kata checks as written. Through an operator, `ans =
    // ans + cnt[x]`, the literal keeps its default and gets its suffix.
    let src = "fn pairs(nums: Vec[i32]) -> i32 {\n    let mut cnt = [0; 4];\n    let mut ans: i32 = 0;\n    for x in nums {\n        ans += cnt[x as i64];\n        cnt[x as i64] += 1;\n    }\n    ans\n}\n\nfn main() {\n    println(pairs([1, 2, 1, 1]));\n}";
    let (first, text, clean, out) = mend(&scratch("array_fill_compound"), src);
    assert!(first.is_empty() || !first.contains("error"), "{first}");
    assert!(text.contains("let mut cnt = [0; 4];"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "3\n");
    let src = src.replace("ans += cnt[x as i64];", "ans = ans + cnt[x as i64];");
    let (first, text, clean, out) = mend(&scratch("array_fill"), &src);
    assert!(
        first.contains("the elements of `cnt` are 'i64' because the literal it is filled with took the default"),
        "{first}"
    );
    assert!(text.contains("let mut cnt = [0i32; 4];"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "3\n");
}

#[test]
fn kata_2024_vec_filled_literal_under_max_gets_its_suffix() {
    let src = "fn peaks(nums: Vec[i32]) -> i32 {\n    let n = nums.len();\n    let mut pre = Vec.filled(n, 0);\n    pre[0] = nums[0];\n    for i in 1..n {\n        pre[i] = pre[i - 1].max(nums[i]);\n    }\n    pre[n - 1] as i32\n}\n\nfn main() {\n    println(peaks([3, 9, 4]));\n}";
    let (first, text, clean, out) = mend(&scratch("vec_filled"), src);
    assert!(first.contains("write that literal `0i32`"), "{first}");
    assert!(
        text.contains("let mut pre = Vec.filled(n, 0i32);"),
        "{text}"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "9\n");
}

#[test]
fn kata_2024_a_literal_that_does_not_fit_gets_no_suffix() {
    let src = "fn f(y: i8) -> i8 {\n    let mut (a, b) = (300, 0);\n    a = a.max(y);\n    a as i8\n}\n\nfn main() {\n    println(f(1));\n}";
    let (first, text, _, _) = mend(&scratch("no_fit"), src);
    assert!(
        first.contains("`max` expects an argument of type `i64`, got `i8`"),
        "{first}"
    );
    assert!(!first.contains("write that literal"), "{first}");
    assert!(text.contains("(300, 0)"), "{text}");
}
