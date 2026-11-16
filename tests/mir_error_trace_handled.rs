//! An `Err` the program consumed by anything other than `?` ends its
//! error return trace (design.md § Error return traces): a `match`,
//! `if let` or `let ... else` on the `Result`, or a method that consumes it
//! without panicking (`unwrap_or`, `is_err`, `ok`), clears the buffer, so a
//! later panic prints only its own place. A method that carries the error
//! on (`map_err`) keeps it, as does `unwrap` (TK2).
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

/// `src` written as `main.kara` in a fresh directory, after `karac check`
/// accepted it.
fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-error-trace-{name}-{}", std::process::id()));
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

/// `stderr` of `src` on both backends, each `{f}` in it the file's path.
fn reports_everywhere(name: &str, src: &str, code: i32, stderr: &str) {
    let file = program(name, src);
    let want = stderr.replace("{f}", &file.display().to_string());
    for cmd in ["__mir-run", "__mir-llvm-run"] {
        if let Some((c, got)) = run(&file, cmd) {
            assert_eq!(c, Some(code), "{name}: {cmd}: {got}");
            assert_eq!(got, want, "{name}: {cmd}");
        }
    }
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

const PARSE: &str = r#"enum E { Bad(i64) }
fn parse(n: i64) -> Result[i64, E] { if n > 2 { Err(E.Bad(n)) } else { Ok(n) } }
fn twice(n: i64) -> Result[i64, E] { let v = parse(n)?; Ok(v * 2) }
"#;

/// The probe's program: the `Err` is matched and printed, and the index
/// panic after it prints only its own place.
#[test]
fn mir_error_trace_is_cleared_by_a_match_on_the_err() {
    let src = format!(
        "{PARSE}fn main() {{
    match twice(5) {{ Ok(v) => println(f\"ok {{v}}\"), Err(E.Bad(n)) => println(f\"bad {{n}}\") }}
    let v: Vec[i64] = [1, 2];
    println(v[5]);
}}
"
    );
    reports_everywhere(
        "match",
        &src,
        101,
        "runtime error: index out of bounds\n  at {f}:7:13\n",
    );
}

/// `if let`, `let ... else`, `is_err`, `ok` and `unwrap_or` each end the
/// trace; `map_err` carries it to the `unwrap` that panics, which prints
/// only the last chain's frame.
#[test]
fn mir_error_trace_is_cleared_by_each_consuming_form() {
    let src = format!(
        "{PARSE}fn try_else() {{
    let Ok(_) = twice(8) else {{ println(\"else\"); return; }};
}}
fn main() {{
    if let Err(E.Bad(n)) = twice(7) {{ println(f\"if-let {{n}}\") }}
    try_else();
    let b = twice(9).is_err();
    println(f\"{{b}}\");
    let o = twice(9).ok();
    println(f\"{{o.is_none()}}\");
    let a = twice(6).unwrap_or(0);
    println(f\"{{a}}\");
    let w = twice(5).map_err(|e| e);
    w.unwrap();
}}
"
    );
    reports_everywhere(
        "forms",
        &src,
        101,
        "runtime error: called unwrap() on Err\n  at {f}:17:5\nError return trace:\n  {f}:3:46  parse(n)?\n  {f}:17:5\n",
    );
}
