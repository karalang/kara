//! Kata 2098's prefix-sum arm splits the numbers with
//! `let (odds, evens): (Vec[i64], Vec[i64]) = nums.iter().partition(..)`.
//! `partition` keeps its items as they come (library/iterators.md, "Items
//! read through a reference"), `ref i64` from `iter()`, so the halves are
//! `Vec[ref i64]` and the declaration is refused, which is right; but the
//! refusal carried no repair. `karac fix` now writes `.cloned()` before
//! the `partition`, which copies each item.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path.to_str().unwrap().to_string()
}

#[test]
fn kata_2098_partition_of_views_gets_cloned_fix() {
    let src = "fn main() {
    let nums = [4, 1, 5, 3, 8].to_vec();
    let (odds, evens): (Vec[i64], Vec[i64]) = nums.iter().partition(|x| x % 2 == 1);
    let words = [\"a\".to_string(), \"bb\".to_string()].to_vec();
    let (short, long): (Vec[String], Vec[String]) = words.iter().partition(|w| w.len() < 2);
    println(f\"{odds} {evens} {short} {long}\");
}
";
    let file = write("k2098-partition", src);
    let check = karac(&["check", &file]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(stderr.contains("Vec[ref i64]"), "{stderr}");
    let fix = karac(&["fix", &file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(
        fixed.contains("= nums.iter().cloned().partition(|x| x % 2 == 1);")
            && fixed.contains("= words.iter().cloned().partition(|w| w.len() < 2);"),
        "{fixed}"
    );
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "[1, 5, 3] [4, 8] [a] [bb]\n");
}

/// Halves declared as views need nothing, and get nothing.
#[test]
fn kata_2098_partition_into_views_is_left_alone() {
    let src = "fn main() {
    let nums = [4, 1, 5].to_vec();
    let (odds, evens): (Vec[ref i64], Vec[ref i64]) = nums.iter().partition(|x| x % 2 == 1);
    println(f\"{odds.len()} {evens.len()}\");
}
";
    let out = karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "2 1\n");
}
