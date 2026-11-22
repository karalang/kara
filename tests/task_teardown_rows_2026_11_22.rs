//! Task teardown (core-semantics §11.9, §11.10, §11.12), on the interpreter
//! and the built binary at one and four workers.
//!
//! - B-2026-11-21-95: a task cancelled before its first poll ran to its first
//!   wait making a group whose task was never cancelled, so the program
//!   waited out that task's whole sleep.
//! - B-2026-11-21-96: on the interpreter, a task that teardown spawned was
//!   never queued, and a group deadline that passed before its task's first
//!   poll ended the run in a false deadlock.
//! - B-2026-11-22-1: an `IsolatingGroup` dropped without a join did not wait
//!   for its tasks, so a task read the frame it borrowed after it was freed
//!   (a cancelled `Server.serve`'s connection task, under AddressSanitizer).

mod common;

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn write(name: &str, src: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-teardown-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.kara"));
    std::fs::write(&path, src).unwrap();
    path
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Runs `src` on the interpreter and as a built binary, each at one and four
/// workers, and asserts each prints `want`.
fn every_lane(name: &str, src: &str, want: &str) {
    let path = write(name, src);
    for threads in ["1", "4"] {
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .env("KARAC_THREADS", threads)
            .arg("__mir-run")
            .arg(&path)
            .output()
            .unwrap();
        assert_eq!(
            (out.status.code(), text(&out.stdout).as_str()),
            (Some(0), want),
            "interpreter, {threads} workers: {}",
            text(&out.stderr)
        );
    }
    // A karac built without the backend has no built lane.
    if !cfg!(feature = "llvm") {
        return;
    }
    let exe = path.with_extension("bin");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("build")
        .arg(&path)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    // Only a failed link may skip the built leg (B-2026-11-21-11).
    if common::build_or_skip(&built).is_none() {
        eprintln!("skipped the built leg: {}", text(&built.stderr));
        return;
    }
    for threads in ["1", "4"] {
        let out = Command::new(&exe)
            .env("KARAC_THREADS", threads)
            .output()
            .unwrap();
        assert_eq!(
            (out.status.code(), text(&out.stdout).as_str()),
            (Some(0), want),
            "built, {threads} workers: {}",
            text(&out.stderr)
        );
    }
}

#[test]
fn task_cancelled_before_its_first_poll_cancels_the_group_it_makes() {
    every_lane(
        "cancel_first",
        "import std.time.{Duration, Instant, sleep};
fn inner() -> i64 { defer println(\"inner torn down\"); sleep(Duration.ms(3000)); 0 }
fn outer() -> i64 {
    defer println(\"outer torn down\");
    let g = TaskGroup.new();
    let h = g.spawn(|| inner());
    h.join()
}
fn main() {
    let start = Instant.now();
    let g = TaskGroup.new();
    let h = g.spawn(|| outer());
    g.cancel();
    println(f\"{h.try_join().is_err()}\");
    println(f\"slow={start.elapsed().as_ms() > 1000}\");
}
",
        "inner torn down\nouter torn down\ntrue\nslow=false\n",
    );
}

#[test]
fn deadline_passed_before_the_first_poll_tears_the_task_down() {
    every_lane(
        "deadline_first",
        "import std.time.{Duration, Instant, sleep};
fn spin(ms: i64) { let t = Instant.now(); let mut s = 0; while t.elapsed().as_ms() < ms { s = s + 1; } }
fn inner() -> i64 { defer println(\"inner torn down\"); sleep(Duration.ms(5000)); 0 }
fn outer() -> i64 {
    defer println(\"outer torn down\");
    let g = TaskGroup.new();
    let h = g.spawn(|| inner());
    h.join()
}
fn main() {
    let start = Instant.now();
    let g = TaskGroup.new().with_deadline(Duration.ms(30));
    let h = g.spawn(|| outer());
    spin(100);
    println(f\"{h.try_join().is_err()}\");
    println(f\"slow={start.elapsed().as_ms() > 2000}\");
}
",
        "inner torn down\nouter torn down\ntrue\nslow=false\n",
    );
}

#[test]
fn isolating_group_drop_waits_for_its_tasks() {
    every_lane(
        "isolating_drop",
        "import std.time.{Duration, sleep};
fn work(v: ref Vec[i64]) -> i64 { sleep(Duration.ms(50)); println(f\"sum {v.len()}\"); 0 }
fn run() {
    let v = [1, 2, 3];
    let g = TaskGroup.isolating();
    let _ = g.spawn(|| work(v));
}
fn main() { run(); println(\"after\"); }
",
        "sum 3\nafter\n",
    );
}
