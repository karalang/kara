//! Found by a `karac check` sweep over kara-katas after kata 1139: two
//! katas that checked clean on 11-09 were refused on 11-12.
//!
//! - Kata 2091's differential reads `nums[best]` in a `fold(0, |best, k|
//!   ..)`. The seed is held open while the closure is checked
//!   (B-2026-11-11-61), and an index did not decide it, as a method call
//!   on it does, so the index read an undecided number (B-2026-11-12-13).
//! - Kata 2077 collects `(0..=n).map(|_| Set.new())` into a
//!   `Vec[Set[i64]]`. B-2026-11-11-41 checks a settled item against the
//!   impls whole, and `Set.new()`'s item carries the hasher its display
//!   leaves off (B-2026-11-12-14).

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn check_and_run(tag: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "karac-sweep-fold-hash-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let check = karac(&["check"], &path);
    let run = karac(&["__mir-run"], &path);
    let _ = std::fs::remove_file(&path);
    assert!(
        check.status.success(),
        "{tag}: {}",
        String::from_utf8_lossy(&check.stderr)
    );
    String::from_utf8_lossy(&run.stdout).into_owned()
}

#[test]
fn a_fold_seed_read_as_an_index_in_its_closure_is_an_index() {
    let out = check_and_run(
        "fold",
        "fn main() {
    let nums: Vec[i64] = [5, 2, 8, 1, 9];
    let n = nums.len();
    let lo = (0..n).fold(0, |best, k| if nums[k] < nums[best] { k } else { best });
    let hi = nums.iter().fold(0, |best, x| if *x > nums[best] { best + 1 } else { best });
    println(f\"{lo} {hi}\");
}
",
    );
    assert_eq!(out, "3 2\n");
}

#[test]
fn hashed_collections_made_in_a_map_closure_collect_into_a_vec_of_them() {
    let out = check_and_run(
        "hashed",
        "fn main() {
    let n = 3;
    let mut up: Vec[Set[i64]] = (0..=n).map(|_| Set.new()).collect();
    up[1].insert(7);
    let names: Vec[Map[String, i64]] = (0..2).map(|_| Map.new()).collect();
    println(f\"{up.len()} {up[1].len()} {names.len()}\");
}
",
    );
    assert_eq!(out, "4 1 2\n");
}
