//! A hashed table's insert fills the latest hole a removal left, rather
//! than appending, so a sliding window of keys stays the same size and
//! never compacts (kata 220, 340 and 391, B-2026-11-16-243). The
//! interpreter keeps the runtime's hole policy, so a pinned
//! `KARAC_HASH_SEED` still walks one order on both backends
//! (collections.md § Map, B-2026-10-23-19).
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const PROGRAM: &str = r#"fn main() {
    let mut m: Map[i64, i64] = Map.new();
    let mut s: Set[i64] = Set.new();
    for i in 0..200 {
        m.insert(i, i * 10);
        s.insert(i);
    }
    // Holes, then refills of the latest hole first.
    for i in 0..200 {
        if i % 3 != 0 {
            let _ = m.remove(i);
            let _ = s.remove(i);
        }
    }
    for i in 200..260 {
        m.insert(i, i * 10);
        s.insert(i);
    }
    // A sliding window: remove the oldest, insert a new key, past a compaction.
    for i in 260..900 {
        let _ = m.remove(i - 60);
        m.insert(i, i);
        if i % 2 == 0 {
            let _ = s.remove(i - 61);
        }
        s.insert(i);
    }
    let mut ks = "";
    for (k, v) in m {
        ks = f"{ks} {k}:{v}";
    }
    println(ks);
    let mut ss = "";
    for k in s {
        ss = f"{ss} {k}";
    }
    println(ss);
    println(f"{m.len()} {s.len()}");
}
"#;

/// `karac <cmd> main.kara` under `KARAC_HASH_SEED=<seed>`: its stdout, or
/// `None` when the compiled backend cannot link.
fn run(file: &std::path::Path, cmd: &str, seed: &str) -> Option<String> {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(file)
        .env("KARAC_HASH_SEED", seed)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if cmd == "__mir-llvm-run" && stderr.contains("undefined reference") {
        common::link_or_skip(Err(stderr.clone()))?;
    }
    assert!(out.status.success(), "{cmd}: {stderr}");
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Removals, refills and a window past a compaction leave the same
/// entries in the same walk order on both backends, under two seeds.
#[test]
fn mir_table_refills_walk_one_order_on_both_backends() {
    let dir = std::env::temp_dir().join(format!("karac-slot-reuse-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, PROGRAM).unwrap();
    for seed in ["1", "2"] {
        let interp = run(&file, "__mir-run", seed).unwrap();
        // The entries, whatever their order: a Python model of the
        // program's removals and inserts gives 127 and 448.
        let lines: Vec<&str> = interp.lines().collect();
        assert_eq!(lines.len(), 3, "{interp}");
        assert_eq!(lines[2], "127 448");
        let mut keys: Vec<i64> = lines[0]
            .split_whitespace()
            .map(|kv| kv.split(':').next().unwrap().parse().unwrap())
            .collect();
        keys.sort();
        let want: Vec<i64> = (0..200).filter(|i| i % 3 == 0).chain(840..900).collect();
        assert_eq!(keys, want);
        if let Some(compiled) = run(&file, "__mir-llvm-run", seed) {
            assert_eq!(compiled, interp, "seed {seed}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
