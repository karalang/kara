//! `Set.retain`, which `Map`, `SortedMap`, `SortedSet`, `Vec` and
//! `VecDeque` already had (kata 2248, B-2026-11-17-151): Rust's
//! `HashSet::retain` was refused "no method 'retain' on type 'Set'".

use std::process::Command;

const SRC: &str = "fn main() {
    let mut common: Set[i64] = Set.from([1, 2, 3, 4, 5, 6].to_vec());
    let seen: Set[i64] = Set.from([2, 4, 6, 8].to_vec());
    common.retain(|x| seen.contains(x));
    let mut v: Vec[i64] = common.iter().cloned().collect();
    v.sort();
    println(f\"{v}\");
    let mut words: Set[String] = Set.from([\"a\".to_string(), \"bb\".to_string(), \"ccc\".to_string()].to_vec());
    words.retain(|w| w.len() > 1);
    let mut w: Vec[String] = words.iter().cloned().collect();
    w.sort();
    println(f\"{w} {words.len()}\");
}
";

#[test]
fn kata_2248_set_retain_keeps_what_the_predicate_accepts() {
    let dir = std::env::temp_dir().join(format!("karac-2248-retain-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("r.kara");
    std::fs::write(&file, SRC).unwrap();
    let check = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(check.status.success(), "{check:?}");
    let run = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(run.status.success(), "{run:?}");
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "[2, 4, 6]\n[bb, ccc] 2\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
