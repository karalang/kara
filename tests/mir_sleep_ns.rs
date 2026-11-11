//! `std.time.sleep(d)` parked for `d.as_ms()` whole milliseconds, so a
//! `Duration` under a millisecond did not pause at all (B-2026-11-11-33).
//! The library's `__sleep_ns` native pauses by the nanosecond, outside a
//! task and inside one, on the interpreter and when built.

mod common;

use std::path::PathBuf;
use std::process::Command;

/// A 200 µs pause, timed outside a task and inside one. The lower bound
/// fails on a pause truncated to whole milliseconds; the upper on one
/// that reads its count as milliseconds (200 s).
const PAUSE: &str = "import std.sync.TaskGroup;

fn pause() -> i64 {
    let t0 = Clock.monotonic_ns();
    __sleep_ns(200000);
    Clock.monotonic_ns() - t0
}

fn main() {
    let out = pause();
    println(f\"outside a task: {out >= 200000} {out < 1000000000}\");
    {
        let group = TaskGroup.new();
        group.spawn(|| {
            let ns = pause();
            println(f\"in a task: {ns >= 200000} {ns < 1000000000}\");
        });
    }
}
";

const EXPECTED: &str = "outside a task: true true\nin a task: true true\n";

fn scratch(tag: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("karac-sleep-ns-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, PAUSE).unwrap();
    (dir, file)
}

#[test]
fn sleep_ns_pauses_under_a_millisecond_on_the_interpreter() {
    let (dir, file) = scratch("interp");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-run")
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8(out.stdout).unwrap(), EXPECTED);
}

#[cfg(feature = "llvm")]
#[test]
fn sleep_ns_pauses_under_a_millisecond_when_built() {
    let (dir, file) = scratch("built");
    let exe = dir.join("main");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["build", file.to_str().unwrap(), "-o"])
        .arg(&exe)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr).to_string();
    assert!(
        built.status.success() || err.contains("Linker failed"),
        "does not build: {err}"
    );
    let linked = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(err)
    });
    if linked.is_some() {
        for threads in ["1", "4"] {
            let out = Command::new(&exe)
                .env("KARAC_THREADS", threads)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(
                String::from_utf8(out.stdout).unwrap(),
                EXPECTED,
                "{threads} workers"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
