//! An error's `?` frames belong to the error value: they end when an `Err`
//! the program holds goes away unhandled, at the end of its temporary's
//! statement or its binding's scope, so a later error's trace does not
//! begin with them (design.md § Error return traces, review-thread ruling
//! 2026-11-16, B-2026-11-16-223). A `?` or a `return` that carries the
//! error on keeps them, `Copy` `Result`s included, and printing a held
//! `Err` does not end them.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

/// `src` written as `main.kara` in a fresh directory, after `karac check`
/// accepted it.
fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-trace-drops-{name}-{}", std::process::id()));
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

/// The exit code and stderr of `karac <cmd> file`; `None` when the
/// compiled backend cannot link.
fn run(file: &Path, cmd: &str) -> Option<(Option<i32>, String)> {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(file)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if cmd == "__mir-llvm-run" && stderr.contains("undefined reference") {
        common::link_or_skip(Err(stderr.clone()))?;
    }
    Some((out.status.code(), stderr))
}

const LIB: &str = r#"fn step(b: bool) -> Result[i64, String] { if b { Ok(1) } else { Err("bad") } }
fn work(b: bool) -> Result[i64, String] { let x = step(b)?; Ok(x + 1) }
fn step2(b: bool) -> Result[i64, i64] { if b { Ok(1) } else { Err(7) } }
fn work2(b: bool) -> Result[i64, i64] { let x = step2(b)?; Ok(x + 1) }
fn hold2() -> Result[i64, i64] { let r = work2(false); r }
"#;

/// `main`'s `body` after [`LIB`] reports `stderr` and exits with `code` on
/// both backends, each `{f}` in `stderr` the file's path.
fn reports_everywhere(name: &str, ret: &str, body: &str, code: i32, stderr: &str) {
    let src = format!("{LIB}fn main() -> Result[(), {ret}] {{\n{body}\n    Ok(())\n}}\n");
    let file = program(name, &src);
    let want = stderr.replace("{f}", &file.display().to_string());
    for cmd in ["__mir-run", "__mir-llvm-run"] {
        if let Some((c, got)) = run(&file, cmd) {
            assert_eq!(c, Some(code), "{name}: {cmd}: {got}");
            assert_eq!(got, want, "{name}: {cmd}");
        }
    }
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

/// The row's program: the `Err` `work` returned is printed, then dropped at the end of its statement.
#[test]
fn mir_error_trace_ends_with_a_printed_and_dropped_err() {
    reports_everywhere(
        "mir_error_trace_ends_with_a_printed_and_dropped_err",
        "String",
        "    println(f\"{work(false)}\");\n    step(false)?;",
        1,
        "Error: bad\nError return trace:\n  {f}:8:5  step(false)?\n",
    );
}

/// A binding that held an `Err` ends with its block.
#[test]
fn mir_error_trace_ends_with_an_err_binding_at_its_block_end() {
    reports_everywhere(
        "mir_error_trace_ends_with_an_err_binding_at_its_block_end",
        "String",
        "    {\n        let r = work(false);\n        println(f\"{r}\");\n    }\n    step(false)?;",
        1,
        "Error: bad\nError return trace:\n  {f}:11:5  step(false)?\n",
    );
}

/// A binding in a loop body ends with each iteration.
#[test]
fn mir_error_trace_ends_with_an_err_binding_each_iteration() {
    reports_everywhere(
        "mir_error_trace_ends_with_an_err_binding_each_iteration",
        "String",
        "    for i in 0..3 {\n        let r = work(false);\n    }\n    step(false)?;",
        1,
        "Error: bad\nError return trace:\n  {f}:10:5  step(false)?\n",
    );
}

/// A `Result[i64, i64]` has no drop, and its end clears all the same.
#[test]
fn mir_error_trace_ends_with_a_dropped_copy_err() {
    reports_everywhere(
        "mir_error_trace_ends_with_a_dropped_copy_err",
        "i64",
        "    let _ = work2(false);\n    step2(false)?;",
        1,
        "Error: 7\nError return trace:\n  {f}:8:5  step2(false)?\n",
    );
}

/// A panic of a fresh error prints no trace of the dropped one.
#[test]
fn mir_error_trace_of_an_unwrap_omits_a_dropped_err() {
    reports_everywhere(
        "mir_error_trace_of_an_unwrap_omits_a_dropped_err",
        "String",
        "    let _ = work(false);\n    let v = step(false).unwrap();\n    println(v);",
        101,
        "runtime error: called unwrap() on Err\n  at {f}:8:13\n",
    );
}

/// `r?` on a `Copy` `Result` binding carries its frames on: the binding's end does not clear what the `?` returns.
#[test]
fn mir_error_trace_continues_through_a_copied_err_binding() {
    reports_everywhere(
        "mir_error_trace_continues_through_a_copied_err_binding",
        "i64",
        "    let r = work2(false);\n    let v = r?;\n    println(v);",
        1,
        "Error: 7\nError return trace:\n  {f}:4:49  step2(b)?\n  {f}:8:13  r?\n",
    );
}

/// A function that returns the `Err` binding it holds keeps its frames.
#[test]
fn mir_error_trace_continues_through_a_returned_err_binding() {
    reports_everywhere(
        "mir_error_trace_continues_through_a_returned_err_binding",
        "i64",
        "    let v = hold2()?;\n    println(v);",
        1,
        "Error: 7\nError return trace:\n  {f}:4:49  step2(b)?\n  {f}:7:13  hold2()?\n",
    );
}

/// Printing does not end the trace; the binding still holds the error its `unwrap` panics on.
#[test]
fn mir_error_trace_survives_printing_a_held_err() {
    reports_everywhere(
        "mir_error_trace_survives_printing_a_held_err",
        "String",
        "    let r = work(false);\n    println(f\"{r}\");\n    let v = r.unwrap();\n    println(v);",
        101,
        "runtime error: called unwrap() on Err\n  at {f}:9:13\nError return trace:\n  {f}:2:51  step(b)?\n  {f}:9:13\n",
    );
}
