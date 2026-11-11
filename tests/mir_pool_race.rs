//! A compiled `Pool.acquire` whose `__ready` saw a value that another
//! worker took before the call asks `__ready` again, rather than sleep its
//! worker for the whole timeout (B-2026-11-11-47). Two tasks trade one
//! value with a 60 s timeout; before the fix about one run in ten stalled.
#![cfg(feature = "llvm")]

mod common;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SRC: &str = r#"
import std.pool.{Pool, PoolError};
import std.time.{Duration};
import std.sync.{TaskGroup};
struct Conn { id: i64 }
fn conn(id: i64) -> Result[Conn, String] { Ok(Conn { id }) }
fn worker(pool: ref Pool[Conn, String], n: i64) -> i64 {
    let mut got = 0;
    for _ in 0..n {
        match pool.acquire(Duration.ms(60000)) {
            Ok(c) => { got += c.val.id; sleep_ms(0); },
            Err(_) => {},
        }
    }
    got
}
fn main() {
    let pool: Pool[Conn, String] = Pool.new(|| conn(1), 1, 4);
    {
        let group = TaskGroup.new();
        group.spawn(|| println(f"a {worker(pool, 100)}"));
        group.spawn(|| println(f"b {worker(pool, 100)}"));
    }
    println("done");
}
"#;

/// How long one run may take before it counts as stalled: the program
/// runs in milliseconds, and a stall lasts the 60 s timeout.
const DEADLINE: Duration = Duration::from_secs(10);

#[test]
fn mir_pool_acquire_that_loses_a_race_waits_again() {
    let dir = std::env::temp_dir().join(format!("karac-mir-pool-race-{}", std::process::id()));
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
    for run in 0..40 {
        let mut child = Command::new(&exe)
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
                panic!("run {run} stalled past {DEADLINE:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let out = child.wait_with_output().unwrap();
        let text = String::from_utf8(out.stdout).unwrap();
        let mut lines: Vec<&str> = text.lines().collect();
        lines.sort_unstable();
        assert_eq!(lines, ["a 100", "b 100", "done"], "run {run}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
