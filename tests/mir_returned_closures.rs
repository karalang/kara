//! A closure inside the value a body returns escapes (core semantics §9.3,
//! "it, or a value containing it, is returned"), so it captures by move,
//! and a capture of a place that dies with the body outlives it (§9.1):
//! `(0..n).flat_map(|i| (i + 1..n).map(|j| i * 10 + j))` passes check and
//! runs, and prints the same, on the interpreter and the compiled backend
//! (B-2026-10-20-34).
#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::Command;

/// The program's stdout from `karac <cmd> main.kara`, after `karac check`
/// accepted it.
fn checked_and_run(name: &str, src: &str, cmd: &str) -> Option<String> {
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "karac-returned-closures-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let check = Command::new(karac)
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "{name}: check: {}",
        String::from_utf8_lossy(&check.stdout)
    );
    let out = Command::new(karac).arg(cmd).arg(&file).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if cmd == "__mir-llvm-run" {
        common::link_or_skip(if out.status.success() {
            Ok(())
        } else {
            Err(err.clone())
        })?;
    }
    assert!(out.status.success(), "{name}: {cmd}: {err}");
    Some(String::from_utf8(out.stdout).unwrap())
}

fn runs_everywhere(name: &str, src: &str, want: &str) {
    for cmd in ["__mir-run", "__mir-llvm-run"] {
        if let Some(out) = checked_and_run(name, src, cmd) {
            assert_eq!(out, want, "{name}: {cmd}");
        }
    }
}

/// A `Copy` capture of the outer closure's parameter, and of a local of its body.
#[test]
fn mir_inner_closure_in_a_returned_iterator_copies_its_capture() {
    let src = r#"fn main() {
    let n = 4;
    let pairs: Vec[i64] = (0..n).flat_map(|i| (i + 1..n).map(|j| i * 10 + j)).collect();
    println(f"{pairs}");
    let q: Vec[i64] = (0..n).flat_map(|i| { let base = i * 10; (i + 1..n).map(|j| base + j) }).collect();
    println(f"{q}");
}
"#;
    runs_everywhere(
        "copy",
        src,
        "[1, 2, 3, 12, 13, 23]\n[1, 2, 3, 12, 13, 23]\n",
    );
}

/// A non-`Copy` capture (kata 816's `x: String`), and a function returning the
/// iterator (`impl Iterator`) over its parameter or a local.
#[test]
fn mir_inner_closure_in_a_returned_iterator_moves_its_capture() {
    let src = r#"fn parts(s: Str) -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push(s.to_string());
    if s.len() > 1 { v.push(f"{s[0..1]}.{s[1..s.len()]}"); }
    v
}
fn pairs(digits: Str) -> Vec[String] {
    (1..digits.len())
        .flat_map(|cut| {
            parts(digits[0..cut]).into_iter().flat_map(|x| parts(digits[cut..digits.len()]).into_iter().map(|y| f"({x}, {y})"))
        })
        .collect()
}
fn adder(i: i64) -> impl Iterator[Item = i64] { (i + 1..4).map(|j| i * 10 + j) }
fn adder2(i: i64) -> impl Iterator[Item = i64] { let b = i * 100; (i + 1..4).map(|j| b + j) }
fn main() {
    println(f"{pairs("123")}");
    let v: Vec[i64] = adder(1).collect();
    println(f"{v}");
    let w: Vec[i64] = adder2(1).collect();
    println(f"{w}");
}
"#;
    runs_everywhere(
        "move",
        src,
        "[(1, 23), (1, 2.3), (12, 3), (1.2, 3)]\n[12, 13]\n[102, 103]\n",
    );
}

/// A closure the body consumes (`.sum()`, `.collect()`) does not escape, so it
/// still borrows `self` and the parameters.
#[test]
fn mir_closure_consumed_before_the_return_keeps_its_borrow() {
    let src = r#"struct Bag { items: Vec[i64], k: i64 }
impl Bag {
    fn big(self) -> Vec[i64] { self.items.iter().filter(|x| *x > self.k).cloned().collect() }
    fn total(self) -> i64 { self.items.iter().map(|x| x + self.k).sum() }
}
fn s(v: Vec[i64], k: i64) -> i64 { v.iter().map(|x| x * k).sum() }
fn firsts(v: Vec[String]) -> Vec[String] { let pre = "x".to_string(); v.iter().map(|w| f"{pre}{w}").collect() }
fn main() {
    let b = Bag { items: vec![1, 5, 9], k: 4 };
    println(f"{b.big()} {b.total()} {s(b.items, 2)}");
    println(f"{firsts(vec!["a".to_string(), "b".to_string()])}");
}
"#;
    runs_everywhere("consumed", src, "[5, 9] 27 30\n[xa, xb]\n");
}
