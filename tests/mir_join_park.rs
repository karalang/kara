//! A join waits off the executor's queue until its task ends, and the end
//! queues it (B-2026-11-21-1). The shapes that must still wake it, each
//! on a built binary under one worker and four, many runs each, since a
//! lost wakeup is schedule-sensitive: a join on a task that sleeps with
//! nothing else to run (an idle wait, not a deadlock), a joined task that
//! panics in an isolating group, one cancelled by its group's deadline,
//! a join from inside a task, many joins at once, and a join that can
//! never end, which is still reported as a deadlock naming the join.

#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const RUNS: usize = 20;

fn build(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-join-park-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let exe = dir.join("main");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["build", file.to_str().unwrap(), "-o"])
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{tag}: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    exe
}

/// Runs `exe` with `threads` workers, killing it after 20 s.
fn run(exe: &PathBuf, threads: &str) -> Output {
    let mut child = Command::new(exe)
        .env("KARAC_THREADS", threads)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let out = child.wait_with_output().unwrap();
            panic!(
                "{} hung with {threads} worker(s)\n{}",
                exe.display(),
                String::from_utf8_lossy(&out.stdout)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

const WAKES: &str = r#"import std.sync.{TaskFailed, TaskGroup};
import std.time.{Duration, sleep};

fn slow(n: i64) -> i64 {
    sleep(Duration.ms(50));
    n * 2
}

fn boom(n: i64) -> i64 {
    sleep(Duration.ms(5));
    if n > 0 {
        panic("boom");
    }
    n
}

fn forever() -> i64 {
    sleep(Duration.ms(60000));
    0
}

fn nested(i: i64) -> i64 {
    let inner = TaskGroup.new();
    let k = inner.spawn(|| slow(i));
    k.join()
}

fn main() {
    let g = TaskGroup.new();
    let h = g.spawn(|| slow(21));
    println(f"slept {h.join()}");

    let iso = TaskGroup.isolating();
    let p = iso.spawn(|| boom(1));
    match p.join() {
        Ok(v) => println(f"ok {v}"),
        Err(TaskFailed.Panicked { message, location }) => println(f"panicked: {message}"),
        Err(TaskFailed.Cancelled) => println("cancelled"),
    }

    let d = TaskGroup.isolating().with_deadline(Duration.ms(30));
    let c = d.spawn(|| forever());
    match c.join() {
        Ok(v) => println(f"ok {v}"),
        Err(TaskFailed.Cancelled) => println("cancelled by the deadline"),
        Err(_) => println("failed"),
    }

    let outer = TaskGroup.new();
    let mut hs = Vec.new();
    for i in 0..200 {
        hs.push(outer.spawn(|| nested(i)));
    }
    let mut sum = 0;
    for h in hs.into_iter() {
        sum += h.join();
    }
    println(f"sum {sum}");
}
"#;

#[test]
fn a_parked_join_wakes_on_every_way_its_task_ends() {
    let exe = build("wakes", WAKES);
    for threads in ["1", "4"] {
        for i in 0..RUNS {
            let out = run(&exe, threads);
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success(),
                "run {i}, {threads} worker(s): {}\n{stdout}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(
                stdout, "slept 42\npanicked: boom\ncancelled by the deadline\nsum 39800\n",
                "run {i}, {threads} worker(s)"
            );
        }
    }
}

const STUCK: &str = r#"import std.sync.{Receiver, Sender, TaskGroup};
fn wait_on(rx: Receiver[i64]) -> i64 {
    match rx.recv() {
        Some(v) => v,
        None => -1,
    }
}

fn middle(rx: Receiver[i64]) -> i64 {
    let inner = TaskGroup.new();
    let k = inner.spawn(|| wait_on(rx));
    k.join()
}

fn main() {
    let (tx, rx): (Sender[i64], Receiver[i64]) = channel(1);
    let g = TaskGroup.new();
    let h = g.spawn(|| middle(rx));
    println(f"{h.join()}");
    let _ = tx;
}
"#;

#[test]
fn a_join_that_can_never_end_is_still_a_deadlock() {
    let exe = build("stuck", STUCK);
    for threads in ["1", "4"] {
        for i in 0..5 {
            let out = run(&exe, threads);
            let dir = format!("{}/", exe.parent().unwrap().display());
            let err = String::from_utf8_lossy(&out.stderr).replace(&dir, "");
            assert!(!out.status.success(), "run {i}, {threads} worker(s)");
            assert!(
                err.starts_with(
                    "runtime error: deadlock: every task waits on a channel or a task that no \
                     running task can free; the other tasks wait on a channel at main.kara:3:11, \
                     a task at main.kara:12:5\n  at main.kara:19:16"
                ),
                "the report names each join, as before joins waited off the queue, \
                 run {i}, {threads} worker(s): {err}"
            );
        }
    }
}
