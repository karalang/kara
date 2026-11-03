//! B-2026-11-01-44: compiled code takes a walk's remainder `(a * i + c) mod
//! n` by the table header's reciprocal of `n` rather than by a division,
//! and must still visit the entries in the interpreter's seeded order.
//! Tables of every size up to 300, and a few larger ones, each fold their
//! walk into a checksum; a removal leaves holes, and a table that grows
//! between walks makes the header's walk words stale.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"fn fold(acc: i64, k: i64) -> i64 {
    (acc * 1000003 + k) % 2147483647
}
fn walk(n: i64) -> i64 {
    let mut m: Map[i64, i64] = Map.new();
    for i in 0..n { let _ = m.insert(i * 31 + 7, i); }
    let mut acc = n;
    for k in m.keys() { acc = fold(acc, k); }
    for (k, v) in m { acc = fold(acc, k - v); }
    let mut s: Set[i64] = Set.new();
    for i in 0..n { let _ = s.insert(i * 17); }
    for x in s { acc = fold(acc, x); }
    acc
}
fn holed(n: i64) -> i64 {
    let mut m: Map[i64, i64] = Map.new();
    for i in 0..n { let _ = m.insert(i, i * i); }
    let mut i = 0;
    while i < n { let _ = m.remove(i); i = i + 3; }
    let mut acc = 0;
    for (k, v) in m { acc = fold(acc, k + v); }
    let _ = m.insert(n, 1);
    for k in m.keys() { acc = fold(acc, k); }
    acc
}
fn main() {
    let mut acc = 0;
    let mut n = 0;
    while n <= 300 {
        acc = fold(acc, walk(n));
        acc = fold(acc, holed(n));
        n = n + 1;
    }
    println(f"{acc}");
    for big in [4096, 65535, 65537, 100003] {
        println(f"{big} {walk(big)} {holed(big)}");
    }
}
"#;

fn run(cmd: &mut Command, seed: &str) -> String {
    let out = cmd.env("KARAC_HASH_SEED", seed).output().expect("run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf-8")
}

#[test]
fn mir_map_walk_by_reciprocal_keeps_the_interpreters_order() {
    let dir =
        std::env::temp_dir().join(format!("karac-mir-walk-reciprocal-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (src, exe) = (dir.join("walk.kara"), dir.join("walk"));
    std::fs::write(&src, SRC).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("build")
        .arg(&src)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("karac build");
    let ok = if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    };
    if common::link_or_skip(ok).is_none() {
        return;
    }
    let mut seen = Vec::new();
    for seed in ["1", "42", "31337"] {
        let want = run(
            Command::new(env!("CARGO_BIN_EXE_karac"))
                .arg("__mir-run")
                .arg(&src),
            seed,
        );
        assert_eq!(want.lines().count(), 5, "{want}");
        assert_eq!(run(&mut Command::new(&exe), seed), want, "seed {seed}");
        seen.push(want);
    }
    seen.dedup();
    assert!(seen.len() > 1, "three seeds walked one order:\n{}", seen[0]);
    let _ = std::fs::remove_dir_all(&dir);
}
