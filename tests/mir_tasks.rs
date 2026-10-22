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

/// A spawned closure that mutates a capture takes its environment `mut ref`
/// and is still a task: it parks on a full channel and its sibling runs
/// (what Thread B's `par` lowering of B-2026-10-21-68 needs).
#[test]
fn mir_tasks_spawn_a_closure_that_mutates_a_capture() {
    let src = r#"
fn main() {
    let (tx, rx): (Sender[i64], Receiver[i64]) = channel(1);
    let mut log: Vec[i64] = Vec.new();
    let names: Vec[String] = vec!["a".to_string(), "b".to_string()];
    let g = TaskGroup.new();
    let h1 = g.spawn(|| {
        let mut s = 0;
        loop {
            let v = rx.recv().unwrap();
            if v < 0 { break; }
            s = s + v;
        }
        s + names.len() as i64
    });
    let h2 = g.spawn(|| {
        for i in 1..=5 { tx.send(i).unwrap(); log.push(i); }
        tx.send(-1).unwrap();
        log.len() as i64
    });
    let total = h1.join() + h2.join();
    println(f"{total} {names[0]}");
}
"#;
    assert_eq!(stdout_on_both("mut_env", src), "22 a\n");
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
    assert_eq!(i.status.code(), Some(101));
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

/// A deadlock is a panic (core semantics §10.1): a lone `recv` nothing can
/// answer, and a second lock of a held `Mutex`, print the runtime's
/// `error: deadlock:` line and exit 101 on both backends, after the output
/// that came before (B-2026-10-19-56). The interpreter exited 3 with a run
/// error.
#[test]
fn mir_a_deadlock_is_a_panic_on_both_backends() {
    let recv = r#"
fn main() {
    let (tx, rx): (Sender[i64], Receiver[i64]) = channel(1);
    println("start");
    let v = rx.recv();
    println("unreached");
}
"#;
    let lock = r#"
import std.sync.{Mutex};
fn main() {
    let m = Mutex.new(1);
    let a = m.lock();
    println("start");
    let b = m.lock();
    println("unreached");
}
"#;
    for (name, src, what) in [
        ("deadlock-recv", recv, "error: deadlock: every task waits"),
        // A second `lock` waits for the holder, which is this task, so it
        // is the scheduler's every-task-waits deadlock on both backends.
        ("deadlock-lock", lock, "error: deadlock: every task waits"),
    ] {
        let (i, _) = interp_run(name, src);
        assert_eq!(i.status.code(), Some(101), "{name}");
        assert_eq!(String::from_utf8_lossy(&i.stdout), "start\n", "{name}");
        let err = String::from_utf8_lossy(&i.stderr);
        assert!(err.starts_with(what), "{name}: {err}");
        let Some((out, _)) = build_and_run(name, src) else {
            continue;
        };
        assert_eq!(out.status.code(), Some(101), "{name}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "start\n", "{name}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.starts_with(what), "{name}: {err}");
    }
}

/// `with_span` around a closure that suspends runs it as a coroutine with
/// the span active across the pause, on both backends (B-2026-10-20-26):
/// the native would have called it through a function value, which cannot
/// run one. A closure taking its environment by borrow, and one owning a
/// capture that drops after the call, both go through.
#[test]
fn mir_tasks_with_span_runs_a_suspending_closure() {
    let src = r#"
struct Noisy { id: i64 }
impl Drop for Noisy { fn drop(mut ref self) { println(f"drop {self.id}"); } }
fn take(n: own Noisy) -> i64 { n.id + 1 }
fn pause_then_log(n: i64) -> i64 {
    sleep_ms(1);
    Log.info("after-resume");
    n * 2
}
fn main() {
    let s = Span.root("req", 7);
    let r = with_span(s, || pause_then_log(21));
    Log.info("outside");
    println(r);
    let t = Span.root("own", 9);
    let noisy = Noisy { id: 1 };
    let k = with_span(t, || { sleep_ms(1); Log.info("owned"); take(noisy) });
    println(k);
}
"#;
    let out = stdout_on_both("with_span_suspends", src);
    assert_eq!(
        out,
        "[info] after-resume span_id=7\n[info] outside\n42\n[info] owned span_id=9\ndrop 1\n2\n"
    );
}

/// B-2026-10-20-37: span context is per task. A task that runs while
/// another is suspended inside `with_span` does not see that span, and a
/// task spawned inside one inherits it.
#[test]
fn mir_tasks_span_context_is_per_task() {
    let src = r#"
fn a() -> i64 {
    let s = Span.root("a", 7);
    with_span(s, || { sleep_ms(50); Log.info("a-after"); 1 })
}
fn b() -> i64 {
    sleep_ms(10);
    Log.info("b-during");
    2
}
fn child() -> i64 {
    sleep_ms(1);
    Log.info("child");
    3
}
fn main() {
    let g = TaskGroup.new();
    let x = g.spawn(|| a());
    let y = g.spawn(|| b());
    println(x.join() + y.join());
    let p = Span.root("p", 5);
    let z = with_span(p, || { let h = g.spawn(|| child()); h.join() });
    Log.info("after");
    println(z);
}
"#;
    let out = stdout_on_both("span_per_task", src);
    assert_eq!(
        out,
        "[info] b-during
[info] a-after span_id=7
3
[info] child span_id=5
[info] after
3
"
    );
}

/// B-2026-10-21-68: `par` branches run as tasks, so a branch may wait on
/// another. The consumer comes first and the channel holds one message, so
/// running the branches one after another deadlocks (core semantics §11.1);
/// the producer moves its `Sender` in and closes the channel as it ends.
#[test]
fn mir_tasks_par_branches_wait_on_each_other() {
    let src = r#"
fn produce(tx: own Sender[i64]) { for i in 1..4 { tx.send(i).unwrap(); } }
fn consume(rx: own Receiver[i64]) -> i64 {
    let mut s = 0;
    for v in rx.into_iter() { println(v); s += v; }
    s
}
fn main() {
    let (tx, rx): (Sender[i64], Receiver[i64]) = channel(1);
    let (s, _) = par { consume(rx), produce(tx) };
    println(f"sum {s}");
}
"#;
    assert_eq!(stdout_on_both("par_wait", src), "1\n2\n3\nsum 6\n");
}

/// B-2026-10-21-68: in a function that returns a `Result` or `Option`, each
/// `par` task returns its branch wrapped, so a `?` ends that branch alone;
/// once every task is joined the source-earliest error is returned and the
/// other results drop, a `Drop` body included (core semantics §11.7).
#[test]
fn mir_tasks_par_branches_fail_after_every_task_is_joined() {
    let src = r#"
struct Loud { n: i64 }
impl Drop for Loud { fn drop(mut ref self) { println(f"drop {self.n}"); } }
fn loud(n: i64) -> Result[Loud, String] { Ok(Loud { n: n }) }
fn fail(n: i64) -> Result[i64, String] { Err(f"fail {n}") }
fn produce(tx: own Sender[i64], k: i64) -> i64 { for i in 0..k { tx.send(i).unwrap(); } k }
fn sum(rx: own Receiver[i64]) -> Result[i64, String] {
    let mut s = 0;
    for v in rx.into_iter() { s += v; }
    if s > 100 { fail(s) } else { Ok(s) }
}
fn pipe(k: i64) -> Result[i64, String] {
    let (tx, rx): (Sender[i64], Receiver[i64]) = channel(1);
    let (a, b) = par { produce(tx, k), sum(rx)? };
    Ok(a + b)
}
fn three() -> Result[i64, String] {
    let (x, y, z) = par { loud(1)?, fail(2)?, fail(3)? };
    Ok(x.n + y + z)
}
fn pick(k: i64) -> Option[i64] { if k > 0 { Some(k) } else { None } }
fn opt(k: i64) -> Option[i64] {
    let (a, b) = par { pick(k)?, pick(2)? };
    Some(a + b)
}
fn main() {
    println(f"{pipe(5)}");
    println(f"{pipe(20)}");
    println(f"{three()}");
    println(f"{opt(3)} {opt(0)}");
}
"#;
    assert_eq!(
        stdout_on_both("par_fail", src),
        "Ok(15)\nErr(fail 190)\ndrop 1\nErr(fail 2)\nSome(5) None\n"
    );
}

/// B-2026-10-21-68: the `par` conflict check sees through the branches'
/// tasks. Two branches that each send on a channel of their own pass
/// (design.md § Value-rooted resources). A branch that writes a place a sibling uses is refused
/// on both backends as a `par` conflict naming both branches: the borrow
/// check leaves a conflict between sibling branches to it, since their
/// tasks overlap, so this check is the one that refuses it.
#[test]
fn mir_tasks_par_conflicts_are_seen_through_the_tasks() {
    let two = r#"
fn produce(tx: Sender[i64], from: i64, to: i64) -> Result[(), SendError[i64]] {
    for i in from..to { tx.send(i)?; }
    Ok(())
}
fn main() {
    let (tx1, rx1): (Sender[i64], Receiver[i64]) = channel(8);
    let (tx2, rx2): (Sender[i64], Receiver[i64]) = channel(8);
    let (r1, r2) = par { produce(tx1, 0, 4), produce(tx2, 4, 8) };
    println(f"{r1.is_ok()} {r2.is_ok()} {rx1.recv().unwrap()} {rx2.recv().unwrap()}");
}
"#;
    assert_eq!(stdout_on_both("par_two_channels", two), "true true 0 4\n");
    let bump = "fn bump(x: mut ref i64) -> i64 { *x += 1; *x }\n";
    let racy = [
        (
            "par_two_pushes",
            "fn main() {\n    let mut v: Vec[i64] = Vec.new();\n    \
             let (a, b) = par { v.push(1), v.push(2) };\n    println(f\"{v}\");\n}\n"
                .to_string(),
            "at line 3, branches 1 and 2 of the `par` conflict: `mutable borrow of v` and `mutable borrow of v`",
        ),
        (
            "par_push_and_read",
            "fn main() {\n    let mut v: Vec[i64] = Vec.new();\n    \
             let (a, b) = par { v.push(1), v.len() };\n    println(f\"{v} {b}\");\n}\n"
                .to_string(),
            "at line 3, branches 1 and 2 of the `par` conflict: `mutable borrow of v` and `read of v`",
        ),
        (
            "par_mut_ref_and_read",
            format!(
                "{bump}fn main() {{\n    let mut n = 1;\n    \
                 let (a, b) = par {{ bump(mut n), n + 1 }};\n    println(f\"{{a}} {{b}}\");\n}}\n"
            ),
            "at line 4, branches 1 and 2 of the `par` conflict: `mutable borrow of n` and `read of n`",
        ),
    ];
    for (name, src, want) in racy {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("karac-mir-tasks-r-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.kara");
        std::fs::write(&file, &src).unwrap();
        for lane in ["__mir-run", "__mir-llvm-run"] {
            let out = Command::new(env!("CARGO_BIN_EXE_karac"))
                .args([lane, file.to_str().unwrap()])
                .output()
                .unwrap();
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(
                !out.status.success(),
                "{name} on {lane} must be refused: {err}"
            );
            assert!(
                err.contains(want),
                "{name} on {lane}: expected `{want}` in: {err}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// B-2026-10-21-68: two `par` branches that each store a `frozen` handle
/// (`work.push(g)`) take a counted copy of it, as they did when the branches
/// ran in place, rather than both moving the one binding into their tasks.
#[test]
fn mir_tasks_par_branches_each_copy_a_frozen_handle() {
    let src = r#"
shared struct Node { val: i64, mut kids: Vec[Node] }
fn build(depth: i64) -> Node {
    if depth <= 0 { return Node { val: 1, kids: [] }; }
    Node { val: depth, kids: [build(depth - 1), build(depth - 1)] }
}
fn walk(g: Node) -> i64 {
    let mut work: Vec[Node] = Vec.new();
    work.push(g);
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < work.len() {
        let n = work[i as u64];
        acc = acc + n.val;
        for k in n.kids { work.push(k); }
        i = i + 1;
    }
    acc
}
fn main() {
    let g = freeze build(3);
    let (x, y) = par {
        { let mut w: Vec[Node] = Vec.new(); w.push(g); walk(w[0]) },
        { let mut w: Vec[Node] = Vec.new(); w.push(g); walk(w[0]) + 1 },
    };
    println(f"{x} {y} {walk(g)}");
}
"#;
    // `karac build`'s legacy ownership pass refuses passing a `frozen`
    // alias to `push`, so the two MIR runners are checked directly.
    let dir: PathBuf =
        std::env::temp_dir().join(format!("karac-mir-tasks-frozen-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    for lane in ["__mir-run", "__mir-llvm-run"] {
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args([lane, file.to_str().unwrap()])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "19 20 19\n",
            "{lane}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Core semantics §11.7: a `return` in a `par` branch is the same early
/// exit as `?`. It ends its branch, which still runs as a task (so a branch
/// may wait on one that returns), every branch is joined, and the function
/// returns the source-earliest early exit's value while the other values
/// drop. A `return` inside a closure in a branch returns from the closure.
#[test]
fn mir_tasks_par_branch_return_ends_the_branch() {
    let src = r#"
struct Loud { n: i64 }
impl Drop for Loud { fn drop(mut ref self) { println(f"drop {self.n}"); } }
fn first(k: i64) -> i64 {
    let (a, b) = par {
        { let l = Loud { n: 1 }; if k == 0 { return 100; } l.n },
        { if k < 2 { return 200; } 2 },
    };
    a + b
}
fn tried(k: i64) -> Result[i64, String] {
    let (a, b, c) = par {
        { if k == 1 { return Ok(7); } 1 },
        { if k == 2 { return Err("two".to_string()); } 2 },
        { let x: Result[i64, String] = if k == 3 { Err("three".to_string()) } else { Ok(3) }; x? },
    };
    Ok(a + b + c)
}
fn waits(stop: bool) -> i64 {
    let (tx, rx) = channel(1);
    let (a, b) = par {
        { let v = rx.recv().unwrap(); if stop { return v * 100; } v },
        { let _ = tx.send(7); 1 },
    };
    a + b
}
fn earliest(k: i64) -> String {
    let (a, b) = par {
        { if k > 0 { return "first".to_string(); } Loud { n: 10 } },
        { if k > 1 { return "second".to_string(); } Loud { n: 20 } },
    };
    f"{a.n} {b.n}"
}
fn never() -> i64 {
    let (_, b) = par { return 5, 3 };
    b
}
fn unit(k: i64) {
    let (a, b) = par {
        { if k == 0 { return; } 1 },
        { let f = |x: i64| { if x > 2 { return 0; } x }; f(k) },
    };
    println(f"unit {a} {b}");
}
fn nested(k: i64) -> i64 {
    let (a, b) = par {
        { let (x, y) = par { { if k == 1 { return 11; } 1 }, 2 }; x + y },
        { if k == 2 { return 22; } 4 },
    };
    a + b
}
fn looped() -> i64 {
    let (a, b) = par {
        { let mut s = 0; for i in 0..10 { if i == 4 { break; } if i == 1 { continue; } s += i; } s },
        2,
    };
    a + b
}
fn main() {
    for k in 0..3 { println(f"first {k} = {first(k)}"); }
    for k in 0..4 { println(f"tried {k} = {tried(k)}"); }
    println(f"waits {waits(false)} {waits(true)}");
    for k in 0..3 { println(f"earliest {k} = {earliest(k)}"); }
    println(f"never {never()}");
    unit(0);
    unit(5);
    for k in 0..3 { println(f"nested {k} = {nested(k)}"); }
    println(f"looped {looped()}");
}
"#;
    assert_eq!(
        stdout_on_both("par-return", src),
        "drop 1\nfirst 0 = 100\ndrop 1\nfirst 1 = 200\ndrop 1\nfirst 2 = 3\ntried 0 = Ok(6)\ntried 1 = Ok(7)\ntried 2 = Err(two)\ntried 3 = Err(three)\nwaits 8 700\ndrop 20\ndrop 10\nearliest 0 = 10 20\ndrop 20\nearliest 1 = first\nearliest 2 = first\nnever 5\nunit 1 0\nnested 0 = 7\nnested 1 = 11\nnested 2 = 22\nlooped 7\n"
    );
}

/// Core semantics §11.7: `break` and `continue` cannot leave a `par`
/// branch, labeled or not, while one aimed at a loop inside the branch is
/// fine (`mir_tasks_par_branch_return_ends_the_branch`'s `looped`).
#[test]
fn mir_tasks_par_branch_break_is_refused() {
    let src = r#"
fn main() {
    let mut n = 0;
    for i in 0..3 {
        let (a, b) = par {
            { if i == 1 { continue; } 1 },
            { if i == 2 { break; } 2 },
        };
        n += a + b;
    }
    'outer: loop {
        let (a, b) = par { { if n > 0 { break 'outer; } 1 }, 2 };
        n += a + b;
    }
    println(f"{n}");
}
"#;
    let dir: PathBuf =
        std::env::temp_dir().join(format!("karac-mir-tasks-break-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    for cmd in ["check", "__mir-run", "__mir-llvm-run"] {
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args([cmd, file.to_str().unwrap()])
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr).to_string()
            + &String::from_utf8_lossy(&out.stdout);
        assert!(!out.status.success(), "{cmd} must refuse: {err}");
        assert!(
            err.contains("6:27: `continue` may not leave a `par` branch"),
            "{cmd}: {err}"
        );
        if cmd == "check" {
            assert!(err.contains("7:27: `break` may not leave"), "{err}");
            assert!(err.contains("12:41: `break` may not leave"), "{err}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
