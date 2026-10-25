//! `std.time.timeout` (core-semantics §11.10, M4): `timeout(d, f)` runs
//! `f` as a task of a group with a deadline and returns its value, or
//! `Err(TimedOut)` once `d` has passed, the task torn down at its wait.
//! The task is the closure `timeout` was handed, by reference or, when it
//! moves a capture out, by value. Each program runs on both MIR backends.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// How long a program may run before it counts as hung.
const DEADLINE: Duration = Duration::from_secs(20);

/// Runs `cmd` to its end, or kills it at [`DEADLINE`] and fails.
fn run(name: &str, mut cmd: Command) -> Output {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if start.elapsed() > DEADLINE {
            let _ = child.kill();
            let out = child.wait_with_output().unwrap();
            panic!("{name} hung\n{}", String::from_utf8_lossy(&out.stdout));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn scratch(name: &str, tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-mir-timeout-{tag}-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    (dir, file)
}

fn karac(file: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_MIR_COROUTINES", "1").args(args).arg(file);
    c
}

/// The program's stdout on the interpreter.
fn stdout_on_interp(name: &str, src: &str) -> String {
    let (dir, file) = scratch(name, "i", src);
    let out = run(name, karac(&file, &["__mir-run"]));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{name} on the interpreter: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// The program's stdout on the interpreter and, when the runtime archive
/// links, on the built binary, which must print the same.
fn stdout_on_both(name: &str, src: &str) -> String {
    let interp = stdout_on_interp(name, src);

    let (dir, file) = scratch(name, "b", src);
    let exe = dir.join("main");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_MIR_COROUTINES", "1")
        .args(["build", "--mir", file.to_str().unwrap(), "-o"])
        .arg(&exe)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr).to_string();
    assert!(
        built.status.success() || err.contains("Linker failed"),
        "{name} does not build: {err}"
    );
    let linked = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(err)
    });
    if linked.is_some() {
        let out = run(name, Command::new(&exe));
        assert!(
            out.status.success(),
            "{name} built: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            interp,
            "{name}: the build and the interpreter differ"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    interp
}

/// A timed-out call drops the task's locals before `timeout` returns, a
/// call that finishes in time returns its value, and a closure that
/// writes a capture writes the caller's variable.
#[test]
fn mir_timeout_ends_a_slow_call_and_returns_a_fast_one() {
    let src = r#"
import std.time.{Duration, timeout, TimedOut};

struct Noisy {
    name: String,
}

impl Drop for Noisy {
    fn drop(mut ref self) {
        println(f"drop {self.name}");
    }
}

fn slow(ms: i64, tag: String) -> String {
    let n = Noisy { name: f"slow {tag}" };
    sleep_ms(ms);
    f"{tag} done"
}

fn main() {
    let keep = Noisy { name: "keep".to_string() };
    let tag = "a".to_string();
    match timeout(Duration.ms(30), || slow(1000, tag.clone())) {
        Ok(s) => println(f"ok {s}"),
        Err(TimedOut {}) => println("a timed out"),
    }
    match timeout(Duration.ms(2000), || slow(5, "b".to_string())) {
        Ok(s) => println(f"ok {s}"),
        Err(_) => println("b timed out"),
    }
    let mut hits = 0;
    match timeout(Duration.ms(2000), || {
        hits += 1;
        hits
    }) {
        Ok(v) => println(f"ok {v}"),
        Err(_) => println("c timed out"),
    }
    println(f"hits {hits} tag {tag}");
    println(keep.name);
}
"#;
    assert_eq!(
        stdout_on_both("ends", src),
        "drop slow a\na timed out\ndrop slow b\nok b done\nok 1\nhits 1 tag a\nkeep\ndrop keep\n"
    );
}

/// A closure that moves a capture out is handed to `timeout` by value,
/// and its task owns it.
#[test]
fn mir_timeout_runs_a_closure_that_moves_a_capture_out() {
    let src = r#"
import std.time.{Duration, timeout};

fn main() {
    let s = "x".to_string();
    match timeout(Duration.ms(100), || {
        let t = s;
        t.len()
    }) {
        Ok(v) => println(f"ok {v}"),
        Err(_) => println("timed out"),
    }
}
"#;
    assert_eq!(stdout_on_both("moves", src), "ok 1\n");
}
