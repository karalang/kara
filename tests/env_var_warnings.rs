//! A malformed `KARAC_HASH_SEED` or `KARAC_THREADS` warns once on stderr
//! and the default is used; both were ignored without a word
//! (B-2026-11-16-288). The interpreter reads both as the compiled runtime
//! does.

use std::process::Command;

/// Hashes a key (the seed is read at the first hash) and has an auto-par
/// site (the worker count is read where the in-order twin is chosen).
const PROG: &str = "fn squares(n: i64) -> Vec[Vec[i64]] {
    let mut out: Vec[Vec[i64]] = Vec.new();
    let mut i = 0;
    while i < n {
        let mut row: Vec[i64] = Vec.new();
        let mut j = 0;
        while j < i {
            row.push(i * j);
            j = j + 1;
        }
        out.push(row);
        i = i + 1;
    }
    out
}

fn main() {
    let rows = squares(5);
    let mut m: Map[String, i64] = Map.new();
    m.insert(\"a\", 1);
    m.insert(\"b\", 2);
    println(f\"{rows.len()} {m.len()}\");
}
";

/// `karac __mir-run` of `PROG` with `var=value`: stdout and stderr.
fn run_with(tag: &str, var: &str, value: &str) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("karac-envwarn-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("p.kara"), PROG).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .env(var, value)
        .args(["__mir-run", "p.kara"])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn env_malformed_hash_seed_warns_once() {
    for (i, value) in ["abc", "-1", "99999999999999999999"].iter().enumerate() {
        let (out, err) = run_with(&format!("seed{i}"), "KARAC_HASH_SEED", value);
        assert_eq!(out, "5 2\n", "{err}");
        let want = format!("warning: KARAC_HASH_SEED=`{value}` is not a seed");
        assert_eq!(err.matches(&want).count(), 1, "{value}: {err}");
    }
    for (i, value) in ["7", "0x2a", ""].iter().enumerate() {
        let (out, err) = run_with(&format!("seedok{i}"), "KARAC_HASH_SEED", value);
        assert_eq!(out, "5 2\n", "{err}");
        assert!(!err.contains("KARAC_HASH_SEED"), "{value}: {err}");
    }
}

#[test]
fn env_malformed_thread_count_warns_once() {
    for (i, value) in ["abc", "0", "-3"].iter().enumerate() {
        let (out, err) = run_with(&format!("threads{i}"), "KARAC_THREADS", value);
        assert_eq!(out, "5 2\n", "{err}");
        let want = format!("warning: KARAC_THREADS=`{value}` is not a worker count");
        assert_eq!(err.matches(&want).count(), 1, "{value}: {err}");
    }
    let (_, err) = run_with("threadsok", "KARAC_THREADS", "2");
    assert!(!err.contains("KARAC_THREADS"), "{err}");
}
