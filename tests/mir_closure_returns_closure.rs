//! A closure returned from a closure escapes and takes its captures by
//! move (core semantics §9.3), so a non-`Copy` place of the outer
//! closure's that it uses moves out of the outer environment: the outer
//! closure captures that place by move too (§9.1) and is an `OnceFn`
//! (§9.4). A `shared` handle is counted rather than moved, so the outer
//! closure keeps borrowing it (B-2026-10-26-8).
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

/// The program's stdout from `karac <cmd> main.kara`, after `karac check`
/// accepted it; `None` when the compiled backend cannot link.
fn checked_and_run(name: &str, src: &str, cmd: &str) -> Option<String> {
    let dir = std::env::temp_dir().join(format!(
        "karac-closure-returns-closure-{name}-{}",
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
    if cmd != "__mir-run" {
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
    for cmd in ["__mir-run", "__mir-llvm-run", "__mir-llvm-asan"] {
        if let Some(out) = checked_and_run(name, src, cmd) {
            assert_eq!(out, want, "{name}: {cmd}");
        }
    }
}

/// The row's program: `base` moves into `make`, and on into `f`.
#[test]
fn mir_closure_returned_from_a_closure_moves_its_owned_capture() {
    runs_everywhere(
        "vec",
        r#"fn main() {
    let base = vec![1, 2, 3];
    let make = |n: i64| |x: i64| x + n + base.len();
    let f = make(1);
    println(f"{f(0)} {f(10)}");
}
"#,
        "4 14\n",
    );
}

/// A handle stays borrowed and counted, and stays usable after; an owned
/// capture moves through a block tail with an early `return`.
#[test]
fn mir_closure_returned_from_a_closure_counts_a_handle() {
    runs_everywhere(
        "handle",
        r#"shared struct Cell { v: i64 }
fn main() {
    let c = Cell { v: 5 };
    let names = vec!["a".to_string(), "bc".to_string()];
    let make = |n: i64| {
        if n > 100 {
            return |x: i64| x;
        }
        |x: i64| x + n + c.v + names.len()
    };
    let f = make(1);
    println(f"{f(0)} {c.v}");
}
"#,
        "8 5\n",
    );
}
