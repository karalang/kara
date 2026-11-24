//! B-2026-11-24-9 (kata 2475): LeetCode's Rust template counts into a map with
//! `*count.entry(x).or_insert(0) += 1` and then multiplies each count by
//! an `i32`. The literal took the default when the map was built, so the
//! counts are `i64`, and the refusal at the narrowing offered only "write
//! an explicit 'as i32'": `karac fix` stopped with two errors. A loop over
//! `count.values()` or `count.iter()` now traces its value back to that
//! literal, and the fix suffixes it.

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-k2475-{name}-{}", std::process::id()));
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

const TEMPLATE: &str = "struct Solution;

impl Solution {
    pub fn unequal_triplets(nums: Vec<i32>) -> i32 {
        let mut count = std::collections::HashMap::new();
        for &x in &nums {
            *count.entry(x).or_insert(0) += 1;
        }
        let n = nums.len() as i32;
        let (mut ans, mut left) = (0, 0);
        for &c in count.values() {
            ans += left * c * (n - left - c);
            left += c;
        }
        ans
    }
}

fn main() {
    let nums: Vec[i32] = [4i32, 4, 2, 4, 3].to_vec();
    println(Solution.unequal_triplets(nums));
}
";

#[test]
fn a_map_counted_from_a_literal_and_read_by_values_is_carried() {
    let f = program("values", TEMPLATE);
    let (ok, out) = karac(&["fix"], &f);
    assert!(ok, "fix: {out}");
    let fixed = std::fs::read_to_string(&f).unwrap();
    assert!(fixed.contains("or_insert(0i32)"), "{fixed}");
    let (ok, out) = karac(&["check"], &f);
    assert!(ok, "check after fix: {out}");
    let (ok, out) = karac(&["run", "--interp"], &f);
    assert!(ok, "run: {out}");
    assert_eq!(out, "3\n");
}

#[test]
fn a_map_value_read_by_a_tuple_loop_names_the_literal() {
    let f = program(
        "tuple",
        "fn main() {
    let mut count = Map.new();
    count.insert(1, 0);
    count.insert(2, 5);
    let mut total: i32 = 0;
    for (k, c) in count.iter() {
        total = total + c;
    }
    println(total);
}
",
    );
    let (ok, out) = karac(&["check"], &f);
    assert!(!ok, "check accepted the i64 values: {out}");
    assert!(out.contains("the elements of `count` are 'i64'"), "{out}");
    assert!(out.contains("write that literal `0i32`"), "{out}");
}
