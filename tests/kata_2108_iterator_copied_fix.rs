//! Kata 2108's differential first wrote `letters.iter().copied()`, Rust's
//! spelling, and drew "no method 'copied' on type 'VecIter'" with nothing
//! to apply. `Option`'s `.copied()` already had a fix to `.cloned()`
//! (B-2026-10-24-26); an iterator's, which `.cloned()` is in Kāra, now
//! gets the same.

use std::process::{Command, Stdio};

#[test]
fn kata_2108_iterator_copied_becomes_cloned() {
    let src = "fn main() {
    let v = [3, 1].to_vec();
    let w: Vec[i64] = v.iter().copied().collect();
    let first = v.iter().copied().next();
    println(f\"{w} {first ?? 0}\");
}
";
    let path = std::env::temp_dir().join(format!("karac-k2108-copied-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let file = path.to_str().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", file])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.contains("an iterator copies its items out with `.cloned()`"),
        "{stderr}"
    );
    let fix = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["fix", file])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(!fixed.contains("copied"), "{fixed}");
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "[3, 1] 3\n");
}
