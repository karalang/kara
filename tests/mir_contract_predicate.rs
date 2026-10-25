//! A panic raised while a `requires` or `ensures` predicate is evaluated,
//! in the predicate or in a function it calls, is the predicate's panic and
//! reads `contract predicate panicked: ..`, apart from a predicate that is
//! false, `contract violated: ..` (design.md § Contracts rule 2), on the
//! interpreter and the compiled backend alike (B-2026-10-25-23).
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

/// The program's stdout and stderr from `karac <cmd> main.kara`, after
/// `karac check` accepted it; `None` when the compiled backend cannot link.
fn run(name: &str, src: &str, cmd: &str) -> Option<(String, String)> {
    let dir = std::env::temp_dir().join(format!(
        "karac-contract-predicate-{name}-{}",
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
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if cmd == "__mir-llvm-run" && stderr.contains("undefined reference") {
        common::link_or_skip(Err(stderr.clone()))?;
    }
    assert_eq!(out.status.code(), Some(101), "{name}: {cmd}: {stderr}");
    Some((String::from_utf8(out.stdout).unwrap(), stderr))
}

fn reports_everywhere(name: &str, src: &str, out: &str, message: &str) {
    for cmd in ["__mir-run", "__mir-llvm-run"] {
        if let Some((stdout, stderr)) = run(name, src, cmd) {
            assert_eq!(stdout, out, "{name}: {cmd}");
            assert!(
                stderr.contains(&format!("runtime error: {message}\n")),
                "{name}: {cmd}: {stderr}"
            );
        }
    }
}

/// An index out of bounds in a function the predicate calls.
#[test]
fn mir_a_fault_in_a_predicate_s_callee_is_the_predicate_s_panic() {
    let src = r#"fn deref_at(v: ref Vec[i64], i: i64) -> bool { v[i] >= 0 }
fn at(v: ref Vec[i64], i: i64) -> i64 requires deref_at(v, i) { 0 }
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    println(at(v, 0));
    let _ = at(v, 9);
}
"#;
    reports_everywhere(
        "callee",
        src,
        "0\n",
        "contract predicate panicked: index out of bounds",
    );
}

/// A predicate that holds leaves later faults unmarked, and a false one is
/// a violation.
#[test]
fn mir_a_fault_after_a_predicate_is_not_the_predicate_s() {
    let after = r#"fn checked(x: i64) -> i64 requires x > 100 { x }
fn main() {
    let v: Vec[i64] = Vec.new();
    println(checked(200));
    println(v[3]);
}
"#;
    reports_everywhere("after", after, "200\n", "index out of bounds");
    let violated = r#"fn checked(x: i64) -> i64 requires x > 100 { x }
fn main() {
    println(checked(200));
    println(checked(5));
}
"#;
    reports_everywhere(
        "violated",
        violated,
        "200\n",
        "contract violated: requires clause",
    );
}
