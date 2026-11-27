//! The process probe's rows (probe-fuzzers/process), against io.md
//! § Running programs and § Exit and time-random-env.md § env:
//! - B-2026-11-17-60: `karac run` reads an argument that is not UTF-8 with
//!   each invalid sequence as U+FFFD, as a built binary does; it aborted.
//! - B-2026-11-17-61: a `Command` env name that is empty or holds `=` or
//!   NUL is `InvalidInput` and starts nothing; a program the system cannot
//!   run is `PermissionDenied` and a working directory that is a file is
//!   `NotFound`, the kinds io.md lists.
//! - B-2026-11-17-62: a `PATH` the command's own `env` sets finds the
//!   program.
//! - B-2026-11-17-63: an exit code outside `0..=255` ends the process with
//!   1, never wrapping to 0.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A fresh directory for one run.
fn scratch(tag: &str) -> std::path::PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-proc-{}-{tag}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `karac <mode> main.kara` in `dir`: exit code, stdout, stderr.
fn run_in(dir: &std::path::Path, mode: &str, src: &str) -> (Option<i32>, String, String) {
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([mode, "main.kara"])
        .current_dir(dir)
        .output()
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The modes to run: the interpreter, and the compiled program where the
/// backend is built.
fn modes() -> Vec<&'static str> {
    let mut m = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        m.push("__mir-llvm-run");
    }
    m
}

/// No runtime archive here: the E2E suites' soft skip.
fn skipped(mode: &str, err: &str) -> bool {
    mode != "__mir-run" && err.contains("runtime archive")
}

const SHOW: &str =
    "import std.process.{ExitStatus};\nfn show(r: Result[ExitStatus, IoError]) -> String {
    match r {
        Ok(st) => f\"ok {st.code().unwrap()}\",
        Err(IoError.NotFound) => \"not found\".to_string(),
        Err(IoError.PermissionDenied) => \"permission denied\".to_string(),
        Err(IoError.InvalidInput) => \"invalid input\".to_string(),
        Err(IoError.Other(m)) => f\"other {m}\",
        Err(_) => \"some other error\".to_string(),
    }
}
";

#[test]
fn command_env_entries_no_environment_holds_start_nothing() {
    for mode in modes() {
        let dir = scratch("env");
        let marker = dir.join("ran");
        let src = format!(
            "import std.process.{{Command, ExitStatus}};\n{SHOW}fn main() {{
    let touch = \"touch {m}\";
    println(show(Command.new(\"sh\").arg(\"-c\").arg(touch).env(\"A=B\", \"c\").status()));
    println(show(Command.new(\"sh\").arg(\"-c\").arg(touch).env(\"\", \"c\").status()));
    println(show(Command.new(\"sh\").arg(\"-c\").arg(touch).env(\"K\", \"a\\0b\").status()));
    println(show(Command.new(\"sh\").arg(\"-c\").arg(\"exit 0\").env(\"K\", \"v\").status()));
}}
",
            m = marker.display()
        );
        let (code, out, err) = run_in(&dir, mode, &src);
        if skipped(mode, &err) {
            continue;
        }
        assert_eq!(code, Some(0), "{mode}: {err}");
        assert_eq!(
            out, "invalid input\ninvalid input\ninvalid input\nok 0\n",
            "{mode}: {err}"
        );
        assert!(!marker.exists(), "{mode}: a command with a bad env ran");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn spawn_failures_take_the_kinds_io_md_lists() {
    for mode in modes() {
        let dir = scratch("kinds");
        let badelf = dir.join("badelf");
        std::fs::write(&badelf, b"\x7fELFgarbage").unwrap();
        std::fs::set_permissions(&badelf, std::fs::Permissions::from_mode(0o755)).unwrap();
        let file = dir.join("plain.txt");
        std::fs::write(&file, "x").unwrap();
        let src = format!(
            "import std.process.{{Command, ExitStatus}};\n{SHOW}fn main() {{
    println(show(Command.new(\"{b}\").status()));
    println(show(Command.new(\"true\").current_dir(\"{f}\").status()));
    println(show(Command.new(\"no-such-program-kara\").status()));
}}
",
            b = badelf.display(),
            f = file.display()
        );
        let (code, out, err) = run_in(&dir, mode, &src);
        if skipped(mode, &err) {
            continue;
        }
        assert_eq!(code, Some(0), "{mode}: {err}");
        assert_eq!(
            out, "permission denied\nnot found\nnot found\n",
            "{mode}: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_command_path_finds_the_program() {
    for mode in modes() {
        let dir = scratch("path");
        let bin = dir.join("pbin");
        std::fs::create_dir_all(&bin).unwrap();
        let prog = bin.join("myprog_px");
        std::fs::write(&prog, "#!/bin/sh\nexit 7\n").unwrap();
        std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755)).unwrap();
        let src = format!(
            "import std.process.{{Command, ExitStatus}};\n{SHOW}fn main() {{
    println(show(Command.new(\"myprog_px\").env(\"PATH\", \"{b}\").status()));
    println(show(Command.new(\"myprog_px\").current_dir(\"{b}\").status()));
}}
",
            b = bin.display()
        );
        let (code, out, err) = run_in(&dir, mode, &src);
        if skipped(mode, &err) {
            continue;
        }
        assert_eq!(code, Some(0), "{mode}: {err}");
        assert_eq!(out, "ok 7\nnot found\n", "{mode}: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn exit_codes_outside_a_byte_end_with_1() {
    for mode in modes() {
        for (n, want) in [(3, 3), (255, 255), (256, 1), (-1, 1), (300, 1)] {
            let dir = scratch("exit");
            let src = format!(
                "import std.process.{{exit}};\nfn main() {{\n    println(\"pre\");\n    exit({n});\n}}\n"
            );
            let (code, out, err) = run_in(&dir, mode, &src);
            if skipped(mode, &err) {
                break;
            }
            assert_eq!(
                (code, out.as_str()),
                (Some(want), "pre\n"),
                "{mode} exit({n}): {err}"
            );
            let src = format!("fn main() -> ExitCode {{\n    ExitCode.from({n})\n}}\n");
            let (code, _, err) = run_in(&dir, mode, &src);
            assert_eq!(code, Some(want), "{mode} ExitCode.from({n}): {err}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// `karac run` of a program printing its arguments, one not UTF-8.
#[cfg(feature = "llvm")]
#[test]
fn karac_run_reads_a_non_utf8_argument_lossily() {
    use std::os::unix::ffi::OsStrExt;
    let dir = scratch("args");
    std::fs::write(
        dir.join("main.kara"),
        "fn main() {\n    let a = env.args();\n    for x in a.iter().skip(1) {\n        \
         println(f\"[{x}] {x.len()}\");\n    }\n}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["run", "main.kara", "--", "ok"])
        .arg(std::ffi::OsStr::from_bytes(b"a\xffb"))
        .current_dir(&dir)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    if err.contains("runtime archive") || err.contains("karac_jit_runner not found") {
        return;
    }
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "[ok] 2\n[a\u{FFFD}b] 5\n",
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
