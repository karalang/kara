//! Tasks on the compiled MIR backend under the coroutine transform
//! (`KARAC_MIR_COROUTINES=1`, B-2026-10-14-49): a spawn queues its closure
//! as a task, and the runtime's executor (`runtime/src/mir_task.rs`) runs
//! the tasks FIFO, each suspending only while its next blocking call cannot
//! complete.
#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

/// The program built with `--mir` under the coroutine transform and run:
/// its output and how long it ran, or `None` when the runtime archive does
/// not link.
fn build_and_run(name: &str, src: &str) -> Option<(Output, Duration)> {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("karac-mir-tasks-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let exe = dir.join("main");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_MIR_COROUTINES", "1")
        .args([
            "build",
            "--mir",
            file.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr).to_string();
    // Only a failed link may skip; a program that does not compile fails.
    assert!(
        built.status.success() || err.contains("Linker failed"),
        "{name} does not build: {err}"
    );
    common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(err)
    })?;
    let start = Instant::now();
    let out = Command::new(&exe).output().unwrap();
    let took = start.elapsed();
    let _ = std::fs::remove_dir_all(&dir);
    Some((out, took))
}

fn stdout_of(name: &str, src: &str) -> Option<String> {
    let (out, _) = build_and_run(name, src)?;
    assert!(
        out.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8(out.stdout).unwrap())
}

/// Spawned tasks start when the spawner suspends, and interleave at their
/// pauses: both start before either ends.
#[test]
fn mir_tasks_interleave_at_their_pauses() {
    let src = r#"
fn work(name: Str, n: i64) -> i64 { println(f"{name} start"); sleep_ms(50); println(f"{name} end"); n }
fn main() {
    let group = TaskGroup.new();
    let a = group.spawn(|| work("a", 1));
    let b = group.spawn(|| work("b", 2));
    println("spawned both");
    println(f"{a.join() + b.join()}");
}
"#;
    let Some(out) = stdout_of("interleave", src) else {
        return;
    };
    assert_eq!(out, "spawned both\na start\nb start\na end\nb end\n3\n");
}

/// Three tasks pausing 500 ms each finish in about one pause, not three:
/// the executor sleeps once until the nearest deadline.
#[test]
fn mir_tasks_pause_concurrently() {
    let src = r#"
fn work(n: i64) -> i64 { sleep_ms(500); n }
fn main() {
    let group = TaskGroup.new();
    let a = group.spawn(|| work(1));
    let b = group.spawn(|| work(2));
    let c = group.spawn(|| work(3));
    println(f"{a.join() + b.join() + c.join()}");
}
"#;
    let Some((out, took)) = build_and_run("pause", src) else {
        return;
    };
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "6\n");
    assert!(took >= Duration::from_millis(500), "{took:?}");
    // Sequential pauses would take 1500 ms.
    assert!(took < Duration::from_millis(1300), "{took:?}");
}

/// A receiver spawned before its producer waits for the values instead of
/// finding the channel empty.
#[test]
fn mir_tasks_recv_waits_for_a_later_producer() {
    let src = r#"
fn produce(tx: Sender[i64], n: i64) { let mut i = 0; while i < n { tx.send(i).unwrap(); println(f"sent {i}"); i += 1; } }
fn main() {
    let (tx, rx) = Channel.new();
    let group = TaskGroup.new();
    let total = group.spawn(|| {
        let mut s = 0;
        let mut k = 0;
        while k < 3 { let v = rx.recv().unwrap(); println(f"got {v}"); s += v; k += 1; }
        s
    });
    let p = group.spawn(|| produce(tx, 3));
    p.join();
    println(f"total {total.join()}");
}
"#;
    let Some(out) = stdout_of("recv", src) else {
        return;
    };
    assert_eq!(
        out,
        "sent 0\nsent 1\nsent 2\ngot 0\ngot 1\ngot 2\ntotal 3\n"
    );
}

/// Owned results move through their handles; a handle dropped while its
/// task runs leaves the task to drop the result and free the handle.
#[test]
fn mir_tasks_move_owned_results_and_detach_dropped_handles() {
    let src = r#"
struct Noisy { id: i64 }
impl Drop for Noisy {
    fn drop(mut ref self) { println(f"drop {self.id}"); }
}
fn make(i: i64) -> Noisy { sleep_ms(100); Noisy { id: i } }
fn name(i: i64) -> String { sleep_ms(10); f"task-{i}" }
fn main() {
    let group = TaskGroup.new();
    let a = group.spawn(|| name(1));
    let detached = group.spawn(|| make(7));
    let b = group.spawn(|| name(2));
    println(a.join());
    println(b.join());
}
"#;
    let Some(out) = stdout_of("owned", src) else {
        return;
    };
    // `main` returns first, dropping the handle of the slower task, which
    // then drops its result once.
    assert_eq!(out, "task-1\ntask-2\ndrop 7\n");
}

/// Every task waiting on what only another waiting task could provide is a
/// deadlock, reported as the interpreter reports it.
#[test]
fn mir_tasks_report_a_deadlock() {
    let src = r#"
fn main() {
    let (tx, rx) = Channel.new();
    let group = TaskGroup.new();
    let h = group.spawn(|| { let v: i64 = rx.recv().unwrap(); v });
    println(f"{h.join()}");
    tx.send(1).unwrap();
}
"#;
    let Some((out, _)) = build_and_run("deadlock", src) else {
        return;
    };
    assert_eq!(out.status.code(), Some(101));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("deadlock: every task waits"), "{err}");
}

/// The spec's accept loop: a server task waiting in `accept` and `read`
/// lets the client tasks that connect to it run.
#[test]
fn mir_tasks_serve_clients_spawned_after_the_server() {
    let src = r#"
fn serve(listener: TcpListener, n: i64) -> i64 {
    let mut total = 0;
    let mut k = 0;
    while k < n {
        match listener.accept() {
            Ok(conn) => {
                println("accepted");
                let mut buf: Array[u8, 16] = [0u8; 16];
                match conn.read(mut buf) {
                    Ok(m) => total += m,
                    Err(_) => println("read failed"),
                }
            }
            Err(_) => println("accept failed"),
        }
        k += 1;
    }
    total
}
fn client(msg: Str) {
    sleep_ms(50);
    match TcpStream.connect("127.0.0.1:47631".to_string()) {
        Ok(c) => {
            sleep_ms(50);
            match c.write_all(msg.to_string().bytes()) { Ok(_) => println("client sent"), Err(_) => println("send failed") }
        }
        Err(_) => println("connect failed"),
    }
}
fn main() {
    match TcpListener.bind("127.0.0.1:47631".to_string()) {
        Ok(listener) => {
            let group = TaskGroup.new();
            let server = group.spawn(|| serve(listener, 2));
            let a = group.spawn(|| client("abc"));
            let b = group.spawn(|| client("de"));
            a.join();
            b.join();
            println(f"server read {server.join()} bytes");
        }
        Err(_) => println("bind failed"),
    }
}
"#;
    let Some(out) = stdout_of("accept", src) else {
        return;
    };
    let count = |l: &str| out.lines().filter(|x| *x == l).count();
    assert_eq!((count("accepted"), count("client sent")), (2, 2), "{out}");
    assert!(
        out.ends_with(
            "server read 5 bytes
"
        ),
        "{out}"
    );
}
