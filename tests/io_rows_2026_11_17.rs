//! I/O rows of 2026-11-17, on the interpreter and the compiled program:
//! - B-2026-11-17-11: `stdin.lines()` ends after the first `Err` it yields,
//!   as `BufReader.lines()` does, and a later `stdin.lines()` reads on.
//! - B-2026-11-17-8: the empty path `""` names no file: `exists("")` is
//!   false and reading it is `NotFound` (natively here; the wasm half needs
//!   a WASI host and is in io.md's `On wasm_wasi` paragraph).
#![cfg(unix)]

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

fn scratch(tag: &str) -> std::path::PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-io17-{}-{tag}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `karac <mode> main.kara` in a scratch directory with `stdin` piped in:
/// stdout and stderr.
fn run(tag: &str, mode: &str, src: &str, stdin: &[u8]) -> (String, String) {
    let dir = scratch(tag);
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([mode, "main.kara"])
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn modes() -> Vec<&'static str> {
    let mut m = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        m.push("__mir-llvm-run");
    }
    m
}

fn skipped(mode: &str, err: &str) -> bool {
    mode != "__mir-run" && err.contains("runtime archive")
}

#[test]
fn stdin_lines_ends_after_its_first_error() {
    let src = "fn main() {
    for line in stdin.lines() {
        match line {
            Ok(s) => println(f\"ok {s}\"),
            Err(_) => println(\"err\"),
        }
    }
    println(\"again\");
    for line in stdin.lines() {
        match line {
            Ok(s) => println(f\"ok {s}\"),
            Err(_) => println(\"err\"),
        }
    }
}
";
    for mode in modes() {
        let (out, err) = run("lines", mode, src, b"a\n\xff\nb\nc\n");
        if skipped(mode, &err) {
            continue;
        }
        assert_eq!(out, "ok a\nerr\nagain\nok b\nok c\n", "{mode}: {err}");
    }
}

#[test]
fn the_empty_path_names_no_file() {
    let src = "fn main() {
    println(f\"{fs.exists(\"\")}\");
    match fs.read_to_string(\"\") {
        Ok(_) => println(\"read\"),
        Err(IoError.NotFound) => println(\"not found\"),
        Err(_) => println(\"other\"),
    }
}
";
    for mode in modes() {
        let (out, err) = run("empty", mode, src, b"");
        if skipped(mode, &err) {
            continue;
        }
        assert_eq!(out, "false\nnot found\n", "{mode}: {err}");
    }
}
