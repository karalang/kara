//! B-2026-11-18-8 (kata 1609): `Option` had `is_some_and` but not its dual
//! `is_none_or` (Rust 1.82), so `prev.is_none_or(|p| v > p)` was refused
//! with a bare "no method". It is now an `Option` method in the library.

use std::process::{Command, Stdio};

const SRC: &str = "fn rising(v: Slice[i32]) -> bool {
    let mut prev: Option[i32] = None;
    for x in v.iter() {
        if not prev.is_none_or(|p| *x > p) {
            return false;
        }
        prev = Some(*x);
    }
    true
}

fn main() {
    let none: Option[String] = None;
    println(none.is_none_or(|s| s.len() > 3));
    let short = Some(\"ab\".to_string());
    println(short.is_none_or(|s| s.len() > 3));
    let long = Some(\"abcd\".to_string());
    println(long.is_none_or(|s| s.len() > 3));
    println(long.is_some());
    println(rising([1, 3, 5]));
    println(rising([1, 3, 3]));
    println(rising([]));
}
";

const OUT: &str = "true\nfalse\ntrue\ntrue\ntrue\nfalse\ntrue\n";

fn run(lane: &str) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("karac-is-none-or-{lane}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("n.kara"), SRC).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([lane, "n.kara"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn option_is_none_or_runs_in_the_interpreter() {
    let (out, err) = run("__mir-run");
    assert_eq!(out, OUT, "{err}");
}

#[test]
fn option_is_none_or_checks_clean() {
    let (_, err) = run("check");
    assert!(!err.contains("error"), "{err}");
}

#[cfg(feature = "llvm")]
#[test]
fn option_is_none_or_is_lowered_by_llvm() {
    let (out, err) = run("__mir-llvm-run");
    if err.contains("Linker failed") && std::env::var_os("KARAC_REQUIRE_RUNTIME_ARCHIVE").is_none()
    {
        eprintln!("skipping: no runtime archive");
        return;
    }
    assert_eq!(out, OUT, "{err}");
}
