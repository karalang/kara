//! Divergences found by the iterators conformance pass over
//! docs/library/iterators.md and core-types.md.
//!
//! - `flatten` over `Option` or `Result` items told the program to write
//!   `.flat_map(|v| v.into_iter())`, and `karac fix` applied it, but neither
//!   type has an iterator; the hint now keeps the values with `filter_map`.
//! - `step_by` with a step below 1 stepped by 1 going forward, while `rev`
//!   divided by zero (step 0) or stepped by the magnitude (a negative
//!   step); it now panics, on every path (review-thread ruling 2026-11-16).

mod common;

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-iterconf-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    file
}

fn karac(args: &[&str], file: &PathBuf) -> (Option<i32>, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(file)
        .output()
        .unwrap();
    (
        o.status.code(),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

/// The hint for each item type, and the program `karac fix` leaves, run.
#[test]
fn iter_flatten_over_options_and_results_hints_filter_map() {
    let cases = [
        (
            "opt-owned",
            "let a: Vec[Option[i64]] = [Some(1), None, Some(3)];\n    let x: Vec[i64] = a.into_iter().flatten().collect();",
            ".filter_map(|v| v)",
        ),
        (
            "opt-ref",
            "let a: Vec[Option[i64]] = [Some(1), None, Some(3)];\n    let x: Vec[i64] = a.iter().flatten().collect();",
            ".filter_map(|v| v.as_ref())",
        ),
        (
            "res-owned",
            "let a: Vec[Result[i64, String]] = [Ok(1), Err(\"e\".to_string()), Ok(3)];\n    let x: Vec[i64] = a.into_iter().flatten().collect();",
            ".filter_map(|v| v.ok())",
        ),
        (
            "res-ref",
            "let a: Vec[Result[i64, String]] = [Ok(1), Err(\"e\".to_string()), Ok(3)];\n    let x: Vec[i64] = a.iter().flatten().collect();",
            ".filter_map(|v| v.clone().ok())",
        ),
    ];
    for (name, body, spelling) in cases {
        let src = format!("fn main() {{\n    {body}\n    println(f\"{{x}}\");\n}}\n");
        let file = program(name, &src);
        let (_, _, err) = karac(&["check"], &file);
        assert!(
            err.contains(&format!("write `{spelling}` in place of `.flatten()`")),
            "{name}: {err}"
        );
        let _ = karac(&["fix"], &file);
        let fixed = std::fs::read_to_string(&file).unwrap();
        assert!(fixed.contains(spelling), "{name}: fix left\n{fixed}");
        let (code, out, err) = karac(&["__mir-run"], &file);
        assert_eq!(
            (code, out.as_str()),
            (Some(0), "[1, 3]\n"),
            "{name}: {fixed}\n{err}"
        );
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }
}

/// Exit code and stderr of `src` on the interpreter and, under
/// `--features llvm`, compiled.
fn lanes(name: &str, src: &str) -> Vec<(&'static str, Option<i32>, String)> {
    let file = program(name, src);
    let mut cmds = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        cmds.push("__mir-llvm-run");
    }
    let mut out = Vec::new();
    for cmd in cmds {
        let (code, _, err) = karac(&[cmd], &file);
        if cmd == "__mir-llvm-run"
            && err.contains("undefined reference")
            && common::link_or_skip(Err(err.clone())).is_none()
        {
            continue;
        }
        out.push((cmd, code, err));
    }
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
    out
}

#[test]
fn iter_step_by_a_step_below_one_panics_on_every_path() {
    let cases = [
        (
            "range-zero",
            "let r: Vec[i64] = (0..5).step_by(0).collect();\n    println(f\"{r}\");",
        ),
        (
            "range-rev-zero",
            "let r: Vec[i64] = (0..5).step_by(0).rev().collect();\n    println(f\"{r}\");",
        ),
        (
            "range-rev-neg",
            "let r: Vec[i64] = (0..=5).step_by(-2).rev().collect();\n    println(f\"{r}\");",
        ),
        (
            "vec-neg",
            "let v: Vec[i64] = [1, 2, 3];\n    println(v.iter().step_by(-1).count());",
        ),
        (
            "loop-zero",
            "let k = 0;\n    for i in (0..5).step_by(k) { println(i); }",
        ),
        ("loop-neg", "for i in (0..=5).step_by(-3) { println(i); }"),
    ];
    for (name, body) in cases {
        let src = format!("fn main() {{\n    println(\"start\");\n    {body}\n}}\n");
        for (lane, code, err) in lanes(name, &src) {
            assert_eq!(code, Some(101), "{name} {lane}: {err}");
            assert!(
                err.contains("step_by: step must be positive"),
                "{name} {lane}: {err}"
            );
        }
    }
}
