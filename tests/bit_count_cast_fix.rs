//! B-2026-11-14-14: a bit count (`count_ones`, `leading_zeros`, ...) is
//! `i64` in Kāra where Rust's is `u32`, so a Rust port that keeps counts
//! in a `u32` (kata 1558's LeetCode template) was refused twice with no
//! fix-it. A count never exceeds its receiver's width, so `as u32` on the
//! call is the fix, in an assignment and in a `max` argument alike; a
//! target too narrow for the width keeps the plain refusal.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn scratch(tag: &str, src: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("karac-bitcount-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    (dir, f)
}

const KATA_1558: &str = "fn min_operations(nums: Vec[i32]) -> i32 {
    let mut ones: u32 = 0;
    let mut max_bits: u32 = 0;
    for x in nums.iter() {
        ones += x.count_ones();
        if x > 0 {
            max_bits = max_bits.max(32 - x.leading_zeros());
        }
    }
    (ones + max_bits.saturating_sub(1)) as i32
}

fn main() {
    println(min_operations([1, 5]));
    println(min_operations([2, 2]));
    println(min_operations([4, 2, 5]));
    println(min_operations([0]));
}
";

#[test]
fn a_bit_count_into_a_u32_is_fixed_with_as_u32() {
    let (dir, f) = scratch("u32", KATA_1558);
    let p = f.to_str().unwrap();
    let out = karac(&["fix", p]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after = std::fs::read_to_string(&f).unwrap();
    assert!(after.contains("ones += x.count_ones() as u32;"), "{after}");
    assert!(
        after.contains("max_bits.max(32 - x.leading_zeros() as u32)"),
        "{after}"
    );
    let check = karac(&["check", p]);
    assert!(
        check.status.success(),
        "{after}\n{}",
        String::from_utf8_lossy(&check.stdout)
    );
    let run = karac(&["run", "--interp", p]);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "5\n3\n6\n0\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_bit_count_too_wide_for_the_target_keeps_the_plain_refusal() {
    let src =
        "fn main() {\n    let x: i128 = 7;\n    let c: i8 = x.count_ones();\n    println(c);\n}\n";
    let (dir, f) = scratch("i8", src);
    let p = f.to_str().unwrap();
    let check = karac(&["check", p]);
    let text = String::from_utf8_lossy(&check.stdout).into_owned()
        + &String::from_utf8_lossy(&check.stderr);
    assert!(!check.status.success(), "{text}");
    assert!(text.contains("to acknowledge the truncation"), "{text}");
    assert!(!text.contains("a bit count is"), "{text}");
    let _ = karac(&["fix", p]);
    let after = std::fs::read_to_string(&f).unwrap();
    assert!(after.contains("let c: i8 = x.count_ones();"), "{after}");
    let _ = std::fs::remove_dir_all(&dir);
}
