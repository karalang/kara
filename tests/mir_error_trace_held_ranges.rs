//! A held `Err` dropped on an exit that carries another error out (a `?`
//! or a `return Err(..)`) takes only its own frames with it: each frame is
//! numbered, the call that gave the held value its error is bracketed by
//! two marks, and the exit clears the frames between them (design.md §
//! Error return traces, review-thread ruling 2026-11-16, B-2026-11-17-21).
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

/// `src` written as `main.kara` in a fresh directory, after `karac check`
/// accepted it.
fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-trace-held-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let check = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "{name}: check: {}",
        String::from_utf8_lossy(&check.stdout)
    );
    file
}

/// The exit code, stdout and stderr of `karac <cmd> file`; `None` when the
/// compiled backend cannot link.
fn run(file: &Path, cmd: &str) -> Option<(Option<i32>, String, String)> {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(file)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if cmd == "__mir-llvm-run" && stderr.contains("undefined reference") {
        common::link_or_skip(Err(stderr.clone()))?;
    }
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    Some((out.status.code(), stdout, stderr))
}

const LIB: &str = r#"fn step(b: bool) -> Result[i64, String] { if b { Ok(1) } else { Err("bad") } }
fn work(b: bool) -> Result[i64, String] { let x = step(b)?; Ok(x + 1) }
fn both() -> Result[i64, String] { let r = work(false); let v = step(false)?; Ok(v) }
fn ret() -> Result[i64, String] { let r = work(false); if true { return Err("other"); } Ok(1) }
fn shown() -> Result[i64, String] { let r = work(false); println(f"{r}"); let v = step(false)?; Ok(v) }
fn looped() -> Result[i64, String] { for i in 0..3 { let r = work(false); let v = step(i < 2)?; } Ok(0) }
"#;

/// `main`'s `body` after [`LIB`] prints `stdout`, reports `stderr` and
/// exits 1 on both backends, each `{f}` in `stderr` the file's path.
fn reports_everywhere(name: &str, body: &str, stdout: &str, stderr: &str) {
    let src = format!("{LIB}fn main() -> Result[(), String] {{\n{body}\n    Ok(())\n}}\n");
    let file = program(name, &src);
    let want = stderr.replace("{f}", &file.display().to_string());
    for cmd in ["__mir-run", "__mir-llvm-run"] {
        if let Some((c, out, got)) = run(&file, cmd) {
            assert_eq!(c, Some(1), "{name}: {cmd}: {got}");
            assert_eq!(out, stdout, "{name}: {cmd}");
            assert_eq!(got, want, "{name}: {cmd}");
        }
    }
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

/// The row's program: `r1`'s `Err` is still held when `step(false)?` carries
/// another error out, and only the error going out keeps its frames.
#[test]
fn mir_error_trace_held_err_leaves_on_a_question_exit() {
    reports_everywhere(
        "mir_error_trace_held_err_leaves_on_a_question_exit",
        "    let r1 = work(false);\n    let v = step(false)?;\n    println(v);",
        "",
        "Error: bad\nError return trace:\n  {f}:9:13  step(false)?\n",
    );
}

/// The same one call down: the caller's `?` continues the callee's error alone.
#[test]
fn mir_error_trace_held_err_leaves_a_callee_question_exit() {
    reports_everywhere(
        "mir_error_trace_held_err_leaves_a_callee_question_exit",
        "    let v = both()?;\n    println(v);",
        "",
        "Error: bad\nError return trace:\n  {f}:3:65  step(false)?\n  {f}:8:13  both()?\n",
    );
}

/// A `return Err(..)` with an `Err` still held: the returned error has no frames of its own.
#[test]
fn mir_error_trace_held_err_leaves_a_returned_err() {
    reports_everywhere(
        "mir_error_trace_held_err_leaves_a_returned_err",
        "    let v = ret()?;\n    println(v);",
        "",
        "Error: other\nError return trace:\n  {f}:8:13  ret()?\n",
    );
}

/// A shared borrow of the held `Err` (printing it) does not hand it its frames.
#[test]
fn mir_error_trace_held_err_leaves_after_a_shared_borrow() {
    reports_everywhere(
        "mir_error_trace_held_err_leaves_after_a_shared_borrow",
        "    let v = shown()?;\n    println(v);",
        "Err(bad)\n",
        "Error: bad\nError return trace:\n  {f}:5:83  step(false)?\n  {f}:8:13  shown()?\n",
    );
}

/// A binding in a loop body: the iteration whose `?` exits drops only its own held `Err`'s frames.
#[test]
fn mir_error_trace_held_err_leaves_from_a_loop_body() {
    reports_everywhere(
        "mir_error_trace_held_err_leaves_from_a_loop_body",
        "    let v = looped()?;\n    println(v);",
        "",
        "Error: bad\nError return trace:\n  {f}:6:83  step(i < 2)?\n  {f}:8:13  looped()?\n",
    );
}
