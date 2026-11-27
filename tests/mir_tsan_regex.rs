//! A ThreadSanitizer link of a program that uses `Regex`
//! (B-2026-11-15-92) takes the TSan build of the regex archive
//! (`libkarac_runtime_tsan_regex.a`, CLAUDE.md § Runtime archives), and
//! tasks matching on four workers run clean; a missing archive is refused
//! with its recipe, as for the other opt-in archives, never the linker's raw
//! `undefined reference to karac_regex_*`. The GPU and Arrow opt-ins, which
//! have no TSan build, are refused by name (a unit test in
//! `src/backend/driver.rs`).
#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "karac-mir-tsan-regex-test-{tag}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Whether `cc -fsanitize=thread` links and runs here.
fn tsan_available(dir: &Scratch) -> bool {
    if std::env::var_os("KARAC_SKIP_TSAN_TESTS").is_some() {
        return false;
    }
    let c = dir.0.join("probe.c");
    let exe = dir.0.join("probe");
    std::fs::write(&c, "int main(void){return 0;}\n").expect("write probe");
    Command::new("cc")
        .arg("-fsanitize=thread")
        .arg(&c)
        .arg("-o")
        .arg(&exe)
        .status()
        .is_ok_and(|s| s.success())
        && Command::new(&exe).status().is_ok_and(|s| s.success())
}

fn tsan_run(dir: &Scratch, src: &str) -> Output {
    let path = dir.0.join("main.kara");
    std::fs::write(&path, src).expect("write source");
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-llvm-tsan")
        .arg(&path)
        .env("KARAC_THREADS", "4")
        .output()
        .expect("run karac")
}

const MATCHING_TASKS: &str = r#"import std.sync.{TaskGroup};
import std.regex.{Regex};

fn hits(k: i64) -> i64 {
    let re = Regex.compile("a+b").unwrap();
    let mut n = 0;
    for _ in 0..50 {
        if re.is_match(f"x{k}aab") {
            n = n + 1;
        }
    }
    n
}

fn main() {
    let g = TaskGroup.new();
    let mut hs = Vec.new();
    for k in 0..8 {
        hs.push(g.spawn(|| hits(k)));
    }
    let mut t = 0;
    for h in hs {
        t = t + h.join();
    }
    println(t);
}
"#;

#[test]
fn mir_tsan_matching_tasks_link_the_tsan_regex_archive() {
    let dir = Scratch::new("regex");
    if !tsan_available(&dir) {
        eprintln!("skipping: no TSan-capable cc");
        return;
    }
    let out = tsan_run(&dir, MATCHING_TASKS);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("undefined reference"), "stderr:\n{err}");
    if out.status.code() == Some(3) && err.contains("libkarac_runtime_tsan_regex.a") {
        // The recipe names the archive and how to build it.
        assert!(err.contains("--features regex"), "stderr:\n{err}");
        let _ = common::link_or_skip(Err(err.into_owned()));
        return;
    }
    if out.status.code() == Some(3) && err.contains("Linker failed") {
        let _ = common::link_or_skip(Err(err.into_owned()));
        return;
    }
    assert_eq!(out.status.code(), Some(0), "stderr:\n{err}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "400\n");
}
