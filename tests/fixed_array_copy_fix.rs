//! Rust's array literal carried over, `let mut dp = [0, i32::MIN,
//! i32::MIN]; .. let mut next = dp; .. dp = next;`: in Kāra the literal is
//! a `Vec`, so the copy is a move and `dp`'s reuse after it is refused. The
//! fix was `dp.clone()`, a heap copy per step (kata 1262's Rust arm: 2.60 s
//! against 1.40 s). When the list never changes length and is only indexed,
//! walked, measured or copied whole, the fix is `dp: Array[i32, 3]`, which
//! copies as Rust's array does; otherwise it stays `.clone()`
//! (B-2026-11-20-86).

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

/// The check's diagnostics, the text `karac fix` leaves, and what it
/// prints under the MIR interpreter.
fn mend(name: &str, src: &str) -> (String, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-fixed-array-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(&file, src).unwrap();
    let first = karac(&dir, &["check", "cell.kara"]).2;
    karac(&dir, &["fix", "cell.kara"]);
    let text = std::fs::read_to_string(&file).unwrap();
    let (_, out, _) = karac(&dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (first, text, out)
}

const SRC: &str = "fn max_sum_div_three(nums: Vec[i32]) -> i32 {\n    let mut dp = [0, i32.MIN, i32.MIN];\n    for x in nums.iter() {\n        let mut next = dp;\n        for r in 0..3 {\n            if dp[r] != i32.MIN {\n                let s = dp[r] + x;\n                let k = (s % 3) as usize;\n                next[k] = next[k].max(s);\n            }\n        }\n        dp = next;\n    }\n    dp[0]\n}\n\nfn main() {\n    println(max_sum_div_three([3i32, 6i32, 5i32, 1i32, 8i32].to_vec()));\n}\n";

const GROWS: &str = "fn max_sum_div_three(nums: Vec[i32]) -> i32 {\n    let mut dp = [0, i32.MIN, i32.MIN];\n    for x in nums.iter() {\n        let mut next = dp;\n        for r in 0..3 {\n            if dp[r] != i32.MIN {\n                let s = dp[r] + x;\n                let k = (s % 3) as usize;\n                next[k] = next[k].max(s);\n            }\n        }\n        dp = next;\n        dp.push(0);\n        dp.pop();\n    }\n    dp[0]\n}\n\nfn main() {\n    println(max_sum_div_three([3i32, 6i32, 5i32, 1i32, 8i32].to_vec()));\n}\n";

const LENT: &str = "fn max_sum_div_three(nums: Vec[i32]) -> i32 {\n    let mut dp = [0, i32.MIN, i32.MIN];\n    for x in nums.iter() {\n        let mut next = dp;\n        for r in 0..3 {\n            if dp[r] != i32.MIN {\n                let s = dp[r] + x;\n                let k = (s % 3) as usize;\n                next[k] = next[k].max(s);\n            }\n        }\n        dp = next;\n    }\n    first(dp)\n}\n\nfn first(v: Vec[i32]) -> i32 {\n    v[0]\n}\n\nfn main() {\n    println(max_sum_div_three([3i32, 6i32, 5i32, 1i32, 8i32].to_vec()));\n}\n";

#[test]
fn a_list_literal_copied_whole_becomes_a_fixed_array() {
    let (first, text, out) = mend("copied", SRC);
    assert!(first.contains("`dp: Array[i32, 3]`"), "{first}");
    assert!(
        text.contains("let mut dp: Array[i32, 3] = [0, i32.MIN, i32.MIN];"),
        "{text}"
    );
    assert!(text.contains("let mut next = dp;"), "{text}");
    assert_eq!(out, "18\n");
}

#[test]
fn a_list_literal_that_grows_keeps_the_clone() {
    let (first, text, _) = mend("grows", GROWS);
    assert!(!first.contains("Array["), "{first}");
    assert!(text.contains("let mut next = dp.clone();"), "{text}");
}

#[test]
fn a_list_literal_handed_to_a_function_keeps_the_clone() {
    let (first, text, out) = mend("lent", LENT);
    assert!(!first.contains("Array["), "{first}");
    assert!(text.contains("let mut next = dp.clone();"), "{text}");
    assert_eq!(out, "18\n");
}
