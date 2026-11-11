//! The first worker, which alone polls the root task, does not sleep to a
//! timed wait's deadline while a task another worker's progress made worth
//! polling is queued (B-2026-11-11-66). Between settling its own task and
//! taking that one, the other worker counted as not running, and the first
//! slept the deadline out, so a group's join waited for a semaphore
//! `acquire`'s whole timeout although every task had finished. Kata 1115's
//! differential took 62 s instead of 3 s in about one run in twenty.
//!
//! Each of 20000 groups trades a pair of semaphores with a 5 s timeout. A
//! group runs in microseconds, so one that takes a second waited on a
//! deadline. Before the fix 4 of 10 runs had such a group; the test runs
//! three.
#![cfg(feature = "llvm")]

mod common;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SRC: &str = r#"
import std.sync.{Atomic, TaskGroup};
import std.time.{Instant};

fn spin(k: i64, steps: i64) -> i64 {
    let mut x = k;
    for i in 0..steps {
        x = (x * 31 + i) % 1000003;
    }
    x
}

fn main() {
    let mut seed = 1115;
    let mut slow = 0;
    let sum: Atomic[i64] = Atomic.new(0);
    for round in 0..20000 {
        seed = (seed * 1103515245 + 12345) % 2147483648;
        let n = seed / 65536 % 12 + 1;
        let work = seed / 7 % 20;
        let foo_sem = Semaphore.new(1);
        let bar_sem = Semaphore.new(0);
        let t0 = Instant.now();
        {
            let group = TaskGroup.new();
            group.spawn(|| {
                for _ in 0..n {
                    let _ = foo_sem.acquire(5000);
                    let _ = sum.fetch_add(spin(round, work));
                    bar_sem.release();
                }
            });
            group.spawn(|| {
                for _ in 0..n {
                    let _ = bar_sem.acquire(5000);
                    let _ = sum.fetch_add(spin(round, work) % 7);
                    foo_sem.release();
                }
            });
        }
        if t0.elapsed().as_ms() > 1000 {
            slow += 1;
        }
    }
    println(f"slow groups: {slow} {sum.load() > 0}");
}
"#;

/// How long one run may take: about 2 s on a 4-core host, and each stall
/// adds up to the 5 s timeout.
const DEADLINE: Duration = Duration::from_secs(120);

#[test]
fn mir_semaphore_join_does_not_wait_out_an_acquire_deadline() {
    let dir = std::env::temp_dir().join(format!("karac-mir-sem-idle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
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
    if linked.is_none() {
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    for run in 0..3 {
        // Four workers whatever the host has: the stall needs a pool.
        let mut child = Command::new(&exe)
            .env("KARAC_THREADS", "4")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let start = Instant::now();
        while child.try_wait().unwrap().is_none() {
            if start.elapsed() > DEADLINE {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_dir_all(&dir);
                panic!("run {run} took longer than {DEADLINE:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "run {run}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            "slow groups: 0 true\n",
            "run {run}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
