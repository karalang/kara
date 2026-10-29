//! A closure returned from a closure owns its captures (core semantics
//! §9.3), so the call's result borrows what the outer closure's
//! environment holds, never the outer closure itself:
//! `let make = |n: i64| |x: i64| x + n; fs.push(make(i));` passes check and
//! runs on the interpreter and the compiled backend, while a reference the
//! environment holds still ties the result to its referent
//! (B-2026-10-24-9).
#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};

/// `karac check` and `karac <cmd>` on `src`, each in a fresh directory.
fn karac(name: &str, src: &str, cmd: &str) -> (Output, Output) {
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "karac-closure-results-{name}-{cmd}-{}",
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
    let out = Command::new(karac).arg(cmd).arg(&file).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (check, out)
}

fn runs_everywhere(name: &str, src: &str, want: &str) {
    for cmd in ["__mir-run", "__mir-llvm-run"] {
        let (check, out) = karac(name, src, cmd);
        assert!(
            check.status.success(),
            "{name}: check: {}",
            String::from_utf8_lossy(&check.stdout)
        );
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        if cmd == "__mir-llvm-run"
            && common::link_or_skip(if out.status.success() {
                Ok(())
            } else {
                Err(err.clone())
            })
            .is_none()
        {
            continue;
        }
        assert!(out.status.success(), "{name}: {cmd}: {err}");
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            want,
            "{name}: {cmd}"
        );
    }
}

fn refused(name: &str, src: &str, code: &str) {
    let (check, run) = karac(name, src, "__mir-run");
    let said = String::from_utf8_lossy(&check.stdout).to_string()
        + &String::from_utf8_lossy(&check.stderr);
    assert!(
        !check.status.success() && said.contains(code),
        "{name}: check: {said}"
    );
    let err = String::from_utf8_lossy(&run.stderr);
    assert!(
        !run.status.success() && err.contains(code),
        "{name}: __mir-run: {err}"
    );
}

/// The row's program: each call's closure owns its copy of `n`.
#[test]
fn mir_closure_from_a_closure_is_stored_past_the_call() {
    runs_everywhere(
        "stored",
        "fn main() {\n    let make = |n: i64| |x: i64| x + n;\n    let mut fs: Vec[Fn(i64) -> i64] = Vec.new();\n    let mut i: i64 = 0;\n    while i < 200 {\n        fs.push(make(i));\n        i = i + 1;\n    }\n    println(f\"{fs[100](0)}\");\n}\n",
        "100\n",
    );
}

/// A non-`Copy` argument moves into the returned closure; the outer closure
/// is called again after.
#[test]
fn mir_closure_from_a_closure_owns_its_moved_argument() {
    runs_everywhere(
        "moved",
        "fn main() {\n    let make = |s: own String| |x: i64| x + s.len();\n    let mut fs: Vec[Fn(i64) -> i64] = Vec.new();\n    fs.push(make(\"abc\".to_string()));\n    fs.push(make(\"hello\".to_string()));\n    println(f\"{fs[0](0)} {fs[1](1)}\");\n}\n",
        "3 6\n",
    );
}

/// A reference the outer closure captured is copied into each result, which
/// may live beside the outer closure.
#[test]
fn mir_closure_from_a_closure_copies_a_captured_reference() {
    runs_everywhere(
        "captured_ref",
        "fn main() {\n    let base = vec![1, 2, 3];\n    let r = ref base;\n    let make = |n: i64| |x: i64| x + n + r.len();\n    let f = make(1);\n    let g = make(2);\n    println(f\"{f(0)} {g(10)}\");\n}\n",
        "4 15\n",
    );
}

/// The result still borrows what that reference borrows: writing the
/// referent while it lives is refused.
#[test]
fn mir_closure_from_a_closure_keeps_the_environments_borrow() {
    refused(
        "write_while_held",
        "fn main() {\n    let mut base = vec![1, 2, 3];\n    let r = ref base;\n    let make = |n: i64| |x: i64| x + n + r.len();\n    let f = make(1);\n    base.push(4);\n    println(f\"{f(0)}\");\n}\n",
        "E0516",
    );
}

/// And storing it where its borrow is not kept is refused.
#[test]
fn mir_closure_from_a_closure_holding_a_view_is_not_stored() {
    refused(
        "view_stored",
        "fn main() {\n    let mut fs: Vec[Fn(i64) -> i64] = Vec.new();\n    {\n        let base = vec![1, 2, 3];\n        let r = ref base;\n        let make = |n: i64| |x: i64| x + n + r.len();\n        fs.push(make(1));\n    }\n    println(f\"{fs[0](0)}\");\n}\n",
        "E0509",
    );
}
