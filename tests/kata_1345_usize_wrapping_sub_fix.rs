//! B-2026-11-24-49: `karac fix` retyped a Rust `usize` index to `i64` and
//! kept `i.wrapping_sub(1)` as it was, so the idiom `for j in
//! [i.wrapping_sub(1), i + 1] { if j < n { .. arr[j] } }` no longer
//! skipped 0 - 1: on `usize` it wrapped past every length, on `i64` it is
//! -1, passes `j < n`, and the fixed program died with index out of
//! bounds. The fix now appends `.rem_euclid(i64.MAX)`, which keeps it past
//! every length and leaves a value of 0 or more unchanged, as
//! `checked_sub` and `saturating_sub` get their floor (kata 1345's Rust
//! arm).

use std::process::Command;

#[test]
fn kata_1345_usize_wrapping_sub_stays_past_every_length_once_i64() {
    let src = "fn around(i: usize, arr: &Vec<i32>) -> i32 {
    let n = arr.len();
    let mut total = 0;
    for j in [i.wrapping_sub(1), i + 1] {
        if j < n {
            total += arr[j];
        }
    }
    total
}

fn main() {
    let arr = vec![1, 2, 3];
    println!(\"{} {} {}\", around(0, &arr), around(1, &arr), around(2, &arr));
}
";
    let dir = std::env::temp_dir().join(format!("karac-kata-1345-wrap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    let karac = || Command::new(env!("CARGO_BIN_EXE_karac"));
    for _ in 0..4 {
        karac().arg("fix").arg(&path).output().unwrap();
    }
    let fixed = std::fs::read_to_string(&path).unwrap();
    let check = karac().arg("check").arg(&path).output().unwrap();
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        fixed.contains("for j in [i.wrapping_sub(1).rem_euclid(i64.MAX), i + 1] {"),
        "{fixed}"
    );
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "2 4 2\n", "{fixed}");
}
