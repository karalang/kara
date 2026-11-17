//! `env.set` with an empty name, an `=` in the name, or a NUL in the name or
//! the value panics as a program's panic, `env.set: invalid variable name or
//! value` (time-random-env.md § env): Rust's `set_var` panicked there, which
//! aborted karac itself under the interpreter and killed a compiled program
//! by SIGSEGV with its buffered stdout lost (B-2026-11-17-59).

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// `karac <mode>` on a program that prints, sets `name` to `value`, and
/// prints again: exit code, stdout and stderr.
fn run(mode: &str, name: &str, value: &str) -> (Option<i32>, String, String) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-envset-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = format!(
        "fn main() {{\n    println(\"start\");\n    env.set({name}, {value});\n    \
         println(\"after\");\n}}\n"
    );
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([mode, "main.kara"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn check(mode: &str) {
    for (name, value) in [
        ("\"A=B\"", "\"x\""),
        ("\"\"", "\"x\""),
        ("\"NUL\\0N\"", "\"x\""),
        ("\"NULV\"", "\"a\\0b\""),
    ] {
        let (code, out, err) = run(mode, name, value);
        if mode != "__mir-run" && err.contains("runtime archive") {
            return; // no archive here: the E2E suites' soft skip
        }
        assert_eq!(code, Some(101), "{mode} {name}={value}: {err}");
        assert_eq!(out, "start\n", "{mode} {name}={value}: {err}");
        assert!(
            err.contains("env.set: invalid variable name or value"),
            "{mode} {name}={value}: {err}"
        );
        assert!(!err.contains("set_var"), "{mode} {name}={value}: {err}");
    }
    let (code, out, err) = run(mode, "\"KARA_PROBE_OK\"", "\"v\"");
    assert_eq!(
        (code, out.as_str()),
        (Some(0), "start\nafter\n"),
        "{mode}: {err}"
    );
}

#[test]
fn interp_env_set_invalid_name_panics_as_the_program() {
    check("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_env_set_invalid_name_panics_as_the_program() {
    check("__mir-llvm-run");
}
