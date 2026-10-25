//! The error return trace (design.md § Error return traces): each `?` that
//! returns an error records its place and its text, and each one that
//! succeeds clears them, and the trace is printed with the diagnostic it
//! belongs to, after `main`'s `Error: e` or a panic's place, on the
//! interpreter and the compiled backend alike; `karac build --release`
//! strips it (B-2026-10-25-3, B-2026-10-25-29).
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

const CHAIN: &str = r#"fn inner() -> Result[i64, i64] { Err(7) }
fn middle() -> Result[i64, i64] {
    let v = inner()?;
    Ok(v + 1)
}
fn outer() -> Result[i64, i64] {
    let v = middle()?;
    Ok(v * 2)
}
fn main() -> Result[(), i64] {
    let v = outer()?;
    println(v);
    Ok(())
}
"#;

/// Each `?` the error passed on its way out of `main`, oldest first.
#[test]
fn mir_error_trace_follows_a_question_chain_out_of_main() {
    reports_everywhere(
        "chain",
        CHAIN,
        1,
        "Error: 7\nError return trace:\n  {f}:3:13  inner()?\n  {f}:7:13  middle()?\n  {f}:11:13  outer()?\n",
    );
}

/// An error handled with a `match` prints nothing, and a `?` that succeeds
/// afterwards clears its frames, so a later chain reports only its own.
#[test]
fn mir_error_trace_is_cleared_by_a_question_that_succeeds() {
    let handled = r#"fn inner() -> Result[i64, i64] { Err(7) }
fn outer() -> Result[i64, i64] {
    let v = inner()?;
    Ok(v * 2)
}
fn main() {
    match outer() {
        Ok(v) => println(v),
        Err(e) => println(e),
    }
}
"#;
    reports_everywhere("handled", handled, 0, "");
    let cleared = r#"fn inner(x: i64) -> Result[i64, i64] {
    if x > 1 { return Err(x); }
    Ok(x)
}
fn outer(x: i64) -> Result[i64, i64] {
    let v = inner(x)?;
    Ok(v * 2)
}
fn main() -> Result[(), i64] {
    let _ = outer(5);
    let a = outer(1)?;
    println(a);
    let b = inner(3)?;
    println(b);
    Ok(())
}
"#;
    reports_everywhere(
        "cleared",
        cleared,
        1,
        "Error: 3\nError return trace:\n  {f}:13:13  inner(3)?\n",
    );
}

/// A panic's trace is the error's path that led to it, then its own place.
#[test]
fn mir_error_trace_ends_at_a_panic() {
    let src = r#"fn inner() -> Option[i64] { None }
fn middle() -> Option[i64] {
    let v = inner()?;
    Some(v + 1)
}
fn main() {
    let r = middle();
    println(r.unwrap());
}
"#;
    reports_everywhere(
        "panic",
        src,
        101,
        "runtime error: called unwrap() on None\n  at {f}:8:13\nError return trace:\n  {f}:3:13  inner()?\n  {f}:8:13\n",
    );
}

/// The trace keeps the 64 most recent frames and says it dropped others.
#[test]
fn mir_error_trace_keeps_the_most_recent_64_frames() {
    let src = r#"fn down(n: i64) -> Result[i64, i64] {
    if n == 0 { return Err(0); }
    let v = down(n - 1)?;
    Ok(v)
}
fn main() -> Result[(), i64] {
    let v = down(70)?;
    println(v);
    Ok(())
}
"#;
    let mut want = String::from("Error: 0\nError return trace:\n");
    for _ in 0..63 {
        want.push_str("  {f}:3:13  down(n - 1)?\n");
    }
    want.push_str("  {f}:7:13  down(70)?\n  ... (trace truncated, max 64 frames)\n");
    reports_everywhere("deep", src, 1, &want);
}

/// `karac build` keeps the trace and `karac build --release` strips it.
#[test]
fn mir_error_trace_is_stripped_from_a_release_build() {
    let file = program("release", CHAIN);
    let dir = file.parent().unwrap();
    for (release, want) in [
        (
            false,
            "Error: 7\nError return trace:\n  main.kara:3:13  inner()?\n  main.kara:7:13  middle()?\n  main.kara:11:13  outer()?\n",
        ),
        (true, "Error: 7\n"),
    ] {
        let mut build = Command::new(env!("CARGO_BIN_EXE_karac"));
        build.current_dir(dir).arg("build");
        if release {
            build.arg("--release");
        }
        let built = build.arg("main.kara").output().unwrap();
        let err = String::from_utf8_lossy(&built.stderr).to_string();
        if !built.status.success() {
            // A checkout without the runtime archives skips; anything
            // else is a failure.
            assert!(err.contains("link"), "build (release {release}): {err}");
            if common::link_or_skip(Err(err)).is_none() {
                continue;
            }
            unreachable!("link_or_skip passes only a successful link");
        }
        let out = Command::new(dir.join("main")).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "release {release}");
        assert_eq!(String::from_utf8_lossy(&out.stderr), want, "release {release}");
    }
    let _ = std::fs::remove_dir_all(dir);
}
