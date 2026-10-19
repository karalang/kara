//! Tasks on both MIR backends under the coroutine transform
//! (`KARAC_MIR_COROUTINES=1`, B-2026-10-14-49): a spawn queues its closure
//! as a task, and the executor runs the tasks FIFO, each suspending only
//! while its next blocking call cannot complete. The compiled executor is
//! `runtime/src/mir_task.rs`; the interpreter's is `run_tasks` in
//! `src/mir/interp.rs` (B-2026-10-15-72), and each program's output is
//! checked on both.
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

/// The program run by the MIR interpreter under the coroutine transform
/// (`karac __mir-run`), whose executor spawns tasks as the runtime's does
/// (B-2026-10-14-49): its output and how long it ran.
fn interp_run(name: &str, src: &str) -> (Output, Duration) {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("karac-mir-tasks-i-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let start = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_MIR_COROUTINES", "1")
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    let took = start.elapsed();
    let _ = std::fs::remove_dir_all(&dir);
    (out, took)
}

/// The program's output on the interpreter, which must match the built
/// binary's when the archive links.
fn stdout_on_both(name: &str, src: &str) -> String {
    let (out, _) = interp_run(name, src);
    assert!(
        out.status.success(),
        "{name} on the interpreter: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let interp = String::from_utf8(out.stdout).unwrap();
    if let Some(built) = stdout_of(name, src) {
        assert_eq!(
            built, interp,
            "{name}: the build and the interpreter differ"
        );
    }
    interp
}

/// [`stdout_on_both`] for a program whose tasks race on real sockets or
/// clocks: the two backends must print the same lines, in any order.
fn stdout_on_both_unordered(name: &str, src: &str) -> String {
    let (out, _) = interp_run(name, src);
    assert!(
        out.status.success(),
        "{name} on the interpreter: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let interp = String::from_utf8(out.stdout).unwrap();
    if let Some(built) = stdout_of(name, src) {
        let sorted = |s: &str| {
            let mut v: Vec<String> = s.lines().map(String::from).collect();
            v.sort();
            v
        };
        assert_eq!(
            sorted(&built),
            sorted(&interp),
            "{name}: the build and the interpreter print different lines"
        );
    }
    interp
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
    let out = stdout_on_both("interleave", src);
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
    let (i, took) = interp_run("pause", src);
    assert!(i.status.success(), "{}", String::from_utf8_lossy(&i.stderr));
    assert_eq!(String::from_utf8(i.stdout).unwrap(), "6\n");
    assert!(
        took >= Duration::from_millis(500),
        "interpreter ran in {took:?}"
    );
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
    let out = stdout_on_both("recv", src);
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
    let out = stdout_on_both("owned", src);
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
    let (i, _) = interp_run("deadlock", src);
    assert!(!i.status.success());
    let err = String::from_utf8_lossy(&i.stderr);
    assert!(err.contains("deadlock: every task waits"), "{err}");
    let Some((out, _)) = build_and_run("deadlock", src) else {
        return;
    };
    assert_eq!(out.status.code(), Some(101));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("deadlock: every task waits"), "{err}");
}

/// The spec's accept loop: a server task waiting in `accept` and `read`
/// lets the client tasks that connect to it run, on the interpreter too:
/// the `mut buf` the server reads into is reborrowed for `__ready`, not
/// copied out of the frame (B-2026-10-15-75).
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
    // Whether the second client's send or the second accept prints first
    // is a race between the client's sleep and the server's read.
    let out = stdout_on_both_unordered("accept", src);
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

/// A task body that recurses through its pauses: each call's frame holds
/// the frame of the call it makes, boxed on the compiled backend, and the
/// two tasks still interleave at their pauses.
#[test]
fn mir_tasks_run_a_recursive_coroutine() {
    let src = r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.id}"); } }
fn count(n: i64, label: Str) -> i64 {
    let r = R { id: n };
    sleep_ms(5);
    if n == 0 { return 0; }
    let a = count(n - 1, label);
    println(f"{label} {n} {r.id}");
    a + n
}
fn main() {
    let group = TaskGroup.new();
    let x = group.spawn(|| count(3, "x"));
    let y = group.spawn(|| count(2, "y"));
    println(f"sum {x.join() + y.join()}");
}
"#;
    let out = stdout_on_both("recursive", src);
    let count = |l: &str| out.lines().filter(|x| *x == l).count();
    assert_eq!(count("drop 0"), 2, "{out}");
    assert_eq!(count("drop 3"), 1, "{out}");
    let y_done = out.find("y 2 2").expect("y finishes");
    let x_done = out.find("x 3 3").expect("x finishes");
    assert!(y_done < x_done, "the shorter task finishes first: {out}");
    assert!(out.ends_with("sum 9\n"), "{out}");
}

/// `TaskGroup.new(limit: n)` lets at most `n` of its tasks run at once
/// (B-2026-10-15-30): with a limit of 1, each spawn waits for the task
/// before it, so the pauses run one after another.
#[test]
fn mir_task_group_limit_bounds_running_tasks() {
    let src = r#"
fn work(name: Str) -> i64 { println(f"{name} start"); sleep_ms(150); println(f"{name} end"); 1 }
fn main() {
    let group = TaskGroup.new(limit: 1);
    let a = group.spawn(|| work("a"));
    let b = group.spawn(|| work("b"));
    let c = group.spawn(|| work("c"));
    println(f"{a.join() + b.join() + c.join()}");
}
"#;
    let (i, took) = interp_run("limit_one", src);
    assert!(i.status.success(), "{}", String::from_utf8_lossy(&i.stderr));
    assert_eq!(
        String::from_utf8(i.stdout).unwrap(),
        "a start\na end\nb start\nb end\nc start\nc end\n3\n"
    );
    assert!(
        took >= Duration::from_millis(420),
        "interpreter ran in {took:?}"
    );
    let Some((out, took)) = build_and_run("limit_one", src) else {
        return;
    };
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "a start\na end\nb start\nb end\nc start\nc end\n3\n"
    );
    assert!(took >= Duration::from_millis(420), "ran in {took:?}");
}

/// With a limit of 2, four pauses of 300ms take two rounds: more than one
/// pause and less than four.
#[test]
fn mir_task_group_limit_two_runs_two_at_once() {
    let src = r#"
fn main() {
    let group = TaskGroup.new(limit: 2);
    let mut hs: Vec[TaskHandle[i64]] = Vec.new();
    for i in 0..4 {
        hs.push(group.spawn(|| { sleep_ms(300); i }));
    }
    let mut total = 0;
    for h in hs.into_iter() {
        total = total + h.join();
    }
    println(f"{total}");
}
"#;
    let (i, took) = interp_run("limit_two", src);
    assert!(i.status.success(), "{}", String::from_utf8_lossy(&i.stderr));
    assert_eq!(String::from_utf8(i.stdout).unwrap(), "6\n");
    assert!(
        took >= Duration::from_millis(550),
        "interpreter ran in {took:?}"
    );
    let Some((out, took)) = build_and_run("limit_two", src) else {
        return;
    };
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "6\n");
    assert!(
        took >= Duration::from_millis(550) && took < Duration::from_millis(1100),
        "ran in {took:?}"
    );
}

/// Dropping a group joins every task it started, joined or not, before the
/// scope that owns it goes on (docs/library/concurrency.md § TaskGroup).
#[test]
fn mir_task_group_drop_waits_for_its_tasks() {
    let src = r#"
fn main() {
    {
        let group = TaskGroup.new();
        let _h = group.spawn(|| { sleep_ms(100); println("slow task done"); 1 });
        group.spawn(|| { println("quick task done"); 2 });
        println("scope ends");
    }
    println("after the group");
}
"#;
    let out = stdout_on_both("group_end", src);
    assert_eq!(
        out,
        "scope ends\nquick task done\nslow task done\nafter the group\n"
    );
}

/// A task whose closure moves a capture out takes its environment by value,
/// and still runs as a task: the producer owns the only `Sender`, so the
/// channel closes when it returns and `into_iter` ends.
#[test]
fn mir_task_that_moves_its_sender_closes_the_channel() {
    let src = r#"
fn main() {
    let (tx, rx) = channel(1);
    let group = TaskGroup.new();
    group.spawn(|| {
        let tx = tx;
        for i in 0..3 {
            tx.send(i).unwrap();
            println(f"sent {i}");
        }
    });
    for v in rx.into_iter() {
        println(f"got {v}");
    }
    println("closed");
}
"#;
    let out = stdout_on_both("moved_sender", src);
    assert_eq!(out, "sent 0\ngot 0\nsent 1\ngot 1\nsent 2\ngot 2\nclosed\n");
}
