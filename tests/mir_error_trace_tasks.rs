//! An error return trace whose `?` chain ran across tasks reports that it
//! is unavailable instead of the fragment left on whichever thread printed
//! it (design.md § Error return traces, B-2026-10-25-30), on the
//! interpreter and the compiled backend alike, and a chain that stays in one
//! task keeps its frames while other tasks run.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

/// `src` written as `main.kara` in a fresh directory, after `karac check`
/// accepted it.
fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-error-trace-tasks-{name}-{}",
        std::process::id()
    ));
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

/// The exit code, stdout and stderr of `karac <args> file`; `None` when the
/// compiled backend cannot link.
fn run(file: &Path, args: &[&str]) -> Option<(Option<i32>, String, String)> {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(file)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if args[0] != "__mir-run" && stderr.contains("undefined reference") {
        common::link_or_skip(Err(stderr.clone()))?;
    }
    Some((out.status.code(), stdout, stderr))
}

/// `stderr` of `src` on both backends, each `{f}` in it the file's path.
fn reports_everywhere(name: &str, src: &str, code: i32, stderr: &str) {
    let file = program(name, src);
    let want = stderr.replace("{f}", &file.display().to_string());
    for cmd in ["__mir-run", "__mir-llvm-run"] {
        if let Some((c, _, got)) = run(&file, &[cmd]) {
            assert_eq!(c, Some(code), "{name}: {cmd}: {got}");
            assert_eq!(got, want, "{name}: {cmd}");
        }
    }
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

/// A task's `?` returns the error out of the task, and `main` returns it
/// on with a `?` of the join.
const SPLIT: &str = r#"fn inner() -> Result[i64, i64] { Err(7) }
fn work() -> Result[i64, i64] {
    let v = inner()?;
    Ok(v + 1)
}
fn main() -> Result[(), i64] {
    let g = TaskGroup.new();
    let h = g.spawn(|| work());
    let v = h.join()?;
    println(v);
    Ok(())
}
"#;

const UNAVAILABLE: &str =
    "Error return trace: unavailable (the ? chain was split across parallel tasks)\n";

#[test]
fn mir_error_trace_split_across_tasks_is_unavailable() {
    reports_everywhere("split", SPLIT, 1, &format!("Error: 7\n{UNAVAILABLE}"));
}

/// Tasks whose `?`s succeed leave `main`'s own chain whole, and a `?` that
/// succeeds in a task does not clear `main`'s frames.
#[test]
fn mir_error_trace_within_one_task_keeps_its_frames() {
    let src = r#"fn inner(x: i64) -> Result[i64, i64] {
    if x > 1 { return Err(x); }
    Ok(x)
}
fn outer(x: i64) -> Result[i64, i64] {
    let v = inner(x)?;
    Ok(v * 2)
}
fn main() -> Result[(), i64] {
    let g = TaskGroup.new();
    let a = g.spawn(|| outer(1));
    let b = g.spawn(|| outer(0));
    let s = a.join()? + b.join()?;
    println(s);
    let c = outer(3)?;
    println(c);
    Ok(())
}
"#;
    reports_everywhere(
        "one-task",
        src,
        1,
        "Error: 3\nError return trace:\n  {f}:6:13  inner(x)?\n  {f}:15:13  outer(3)?\n",
    );
}

/// `--output=json` and `jsonl` report the unavailable trace as a trace with
/// no frames and the reason.
#[test]
fn mir_error_trace_unavailable_in_json_output() {
    let file = program("json", SPLIT);
    for lane in [&["run"][..], &["run", "--interp"][..]] {
        let mut args = lane.to_vec();
        args.push("--output=json");
        if let Some((c, out, _)) = run(&file, &args) {
            assert_eq!(c, Some(1), "{lane:?}: {out}");
            assert_eq!(
                out,
                "{\"error\":\"7\"}\n{\"error_return_trace\":{\"frames\":[],\"unavailable\":\"the ? chain was split across parallel tasks\"}}\n",
                "{lane:?}"
            );
        }
        let mut args = lane.to_vec();
        args.push("--output=jsonl");
        if let Some((c, out, _)) = run(&file, &args) {
            assert_eq!(c, Some(1), "{lane:?}: {out}");
            assert_eq!(
                out,
                "{\"type\":\"error\",\"message\":\"7\"}\n{\"type\":\"error_return_trace\",\"frames\":[],\"unavailable\":\"the ? chain was split across parallel tasks\"}\n",
                "{lane:?}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}
