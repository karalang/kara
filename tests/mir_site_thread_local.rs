//! The place of the program's latest call into the library, which an abort
//! in the library reports, is kept per thread: with one pair of globals
//! for the whole pool, every worker wrote the same cache line before each
//! library call (kata 3's auto-par loop ran at 0.19x of its sequential
//! build on 4 workers), and an abort on one worker could report another
//! task's call (B-2026-10-26-7).
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"fn pick(i: i64) -> Option[i64] {
    if i == 37 { None } else { Some(i) }
}
fn work(i: i64) -> i64 {
    let mut s = 0;
    for k in 0..20000 {
        s = s + pick(k % 30).unwrap();
    }
    if i == 3 {
        s = s + pick(37).unwrap();
    }
    s
}
fn main() {
    let g = TaskGroup.new();
    let a = g.spawn(|| work(1));
    let b = g.spawn(|| work(2));
    let c = g.spawn(|| work(3));
    let d = g.spawn(|| work(4));
    println(a.join() + b.join() + c.join() + d.join());
}
"#;

const WANT: &str =
    "runtime error: called unwrap() on None\n  at main.kara:10:17\nError return trace:\n  main.kara:10:17\n";

/// The built program's globals are thread-local, and four tasks on four
/// workers calling `unwrap` at another line do not move the reported place
/// of the one that aborts, built or run on the JIT.
#[test]
fn mir_call_site_is_kept_per_thread() {
    let dir = std::env::temp_dir().join(format!("karac-site-tls-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), SRC).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let built = Command::new(karac)
        .current_dir(&dir)
        .env("KARAC_MIR_LL", dir.join("main.ll"))
        .args(["build", "main.kara", "-o", "main"])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr).to_string();
    if !built.status.success() {
        assert!(err.contains("link"), "build: {err}");
        if common::link_or_skip(Err(err)).is_none() {
            return;
        }
        unreachable!("link_or_skip passes only a successful link");
    }
    let ir = std::fs::read_to_string(dir.join("main.ll")).unwrap();
    for g in [
        "@karac.site = internal thread_local global",
        "@karac.site.len = internal thread_local global",
    ] {
        assert!(ir.contains(g), "{g} missing from the IR");
    }
    for _ in 0..5 {
        let out = Command::new(dir.join("main"))
            .env("KARAC_THREADS", "4")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(101));
        assert_eq!(String::from_utf8_lossy(&out.stderr), WANT, "built");
    }
    let out = Command::new(karac)
        .current_dir(&dir)
        .env("KARAC_THREADS", "4")
        .args(["run", "main.kara"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stderr), WANT, "karac run");
    assert_eq!(out.status.code(), Some(101));
    let _ = std::fs::remove_dir_all(&dir);
}
