//! A counting loop that pushes onto a vector which already holds elements
//! (kata 481's `for _ in 0..s[read] { s.push(next) }`) is reserved for by
//! `mir::presize` at each entry. The reservation compares the room it
//! needs with the spare capacity inline and calls the runtime only to grow
//! (B-2026-11-08-5): it called `karac_mir_vec_reserve` on every entry,
//! 66.3M times in kata 481's bench. The pins: both backends print the
//! program's count, and the fill function's IR tests `%presize.short`
//! (the room against `cap - len`) before the runtime call.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

const SRC: &str = r#"fn magical(n: i64) -> i64 {
    let mut s: Vec[i64] = [1, 2, 2];
    let mut read = 2;
    while s.len() < n {
        let next = 3 - s[s.len() - 1];
        for _ in 0..s[read] {
            s.push(next);
        }
        read += 1;
    }
    let mut ones = 0;
    for i in 0..n {
        if s[i] == 1 {
            ones += 1;
        }
    }
    ones
}
fn main() {
    println(f"{magical(6)} {magical(1000)} {magical(100000)}");
}
"#;

/// LeetCode 481's answers: 3 ones in the first 6, 502 in 1000, 49972 in
/// 100000 (kara-katas leetcode/401-500/481-magical-string).
const EXPECTED: &str = "3 502 49972\n";

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-presize-refill-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn karac() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_AUTO_PAR", "0");
    c
}

/// The lines of function `name`'s body in the IR at `ll`.
fn function(ll: &Path, name: &str) -> Vec<String> {
    let ir = std::fs::read_to_string(ll).unwrap();
    let head = format!("@kara.{name}(");
    ir.lines()
        .skip_while(|l| !(l.starts_with("define") && l.contains(&head)))
        .take_while(|l| *l != "}")
        .map(str::to_string)
        .collect()
}

#[test]
fn a_refilled_vector_reserves_only_when_short() {
    let d = dir();
    let file = d.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let interp = karac()
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&interp.stdout),
        EXPECTED,
        "{}",
        String::from_utf8_lossy(&interp.stderr)
    );
    let exe = d.join("main");
    let ll = d.join("main.ll");
    let built = karac()
        .env("KARAC_MIR_LL", &ll)
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    if common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why)
    })
    .is_none()
    {
        return;
    }
    let ran = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&ran.stdout), EXPECTED);
    let ir = function(&ll, "magical");
    assert!(!ir.is_empty(), "no magical in the IR");
    let short = ir
        .iter()
        .filter(|l| l.trim_start().starts_with("%presize.short") && l.contains("icmp sgt"))
        .count();
    assert_eq!(short, 1, "{}", ir.join("\n"));
    let _ = std::fs::remove_dir_all(&d);
}
