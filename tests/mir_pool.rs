//! The task pool's worker threads (M4 item 10b-3): with `KARAC_THREADS`
//! above 1 a built program runs its tasks on that many threads, a task
//! moving between them at its suspensions. Each program here does its work
//! in tasks that touch shared state (an `Atomic`, a `Mutex`, a channel, a
//! tree of task groups) and prints what the interpreter prints, on one,
//! two and four workers, several times over, and under ASan; a program
//! whose tasks all wait for ever still reports the deadlock.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// How long a program may run before it counts as hung.
const DEADLINE: Duration = Duration::from_secs(60);

/// The worker counts each built program runs on, and how many runs each.
const THREADS: [&str; 3] = ["1", "2", "4"];
const RUNS: usize = 4;

/// Runs `cmd` to its end, or kills it at [`DEADLINE`] and fails.
fn run(name: &str, mut cmd: Command) -> Output {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if start.elapsed() > DEADLINE {
            let _ = child.kill();
            let out = child.wait_with_output().unwrap();
            panic!("{name} hung\n{}", String::from_utf8_lossy(&out.stdout));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn scratch(name: &str, tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-mir-pool-{tag}-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    (dir, file)
}

fn karac(file: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_MIR_COROUTINES", "1").args(args).arg(file);
    c
}

/// The program built, or `None` when the runtime archive does not link.
fn built(name: &str, src: &str) -> Option<(PathBuf, PathBuf)> {
    let (dir, file) = scratch(name, "b", src);
    let exe = dir.join("main");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_MIR_COROUTINES", "1")
        .args(["build", "--mir", file.to_str().unwrap(), "-o"])
        .arg(&exe)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success() || err.contains("Linker failed"),
        "{name} does not build: {err}"
    );
    let linked = common::link_or_skip(if out.status.success() {
        Ok(())
    } else {
        Err(err)
    });
    if linked.is_none() {
        let _ = std::fs::remove_dir_all(&dir);
        return None;
    }
    Some((dir, exe))
}

/// The program's stdout on the interpreter, which the built program must
/// print on every worker count, every run; and under ASan on four workers.
fn same_on_the_pool(name: &str, src: &str) -> String {
    let (dir, file) = scratch(name, "i", src);
    let out = run(name, karac(&file, &["__mir-run"]));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{name} on the interpreter: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let interp = String::from_utf8(out.stdout).unwrap();
    if let Some((dir, exe)) = built(name, src) {
        for threads in THREADS {
            for i in 0..RUNS {
                let mut cmd = Command::new(&exe);
                cmd.env("KARAC_THREADS", threads);
                let out = run(name, cmd);
                assert!(
                    out.status.success(),
                    "{name}, KARAC_THREADS={threads}, run {i}: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                assert_eq!(
                    String::from_utf8(out.stdout).unwrap(),
                    interp,
                    "{name}, KARAC_THREADS={threads}, run {i}: the build and the interpreter differ"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    if std::env::var_os("KARAC_SKIP_ASAN_TESTS").is_none() {
        let (dir, file) = scratch(name, "a", src);
        let mut cmd = karac(&file, &["__mir-llvm-asan"]);
        cmd.env("KARAC_THREADS", "4");
        let out = run(name, cmd);
        let _ = std::fs::remove_dir_all(&dir);
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        if !(out.status.code() == Some(3) && err.contains("Linker failed")) {
            assert!(out.status.success(), "{name} under ASan: {err}");
            assert_eq!(
                String::from_utf8(out.stdout).unwrap(),
                interp,
                "{name} under ASan"
            );
        }
    }
    interp
}

const ATOMIC: &str = r#"import std.sync.{Atomic};

sync struct Counter {
    n: Atomic[i64],
}

fn work(c: Counter, k: i64) -> i64 {
    let mut i = 0;
    let mut s = 0;
    while i < 20000 {
        c.n.fetch_add(1);
        s += i % k;
        i += 1;
    }
    s
}

fn main() {
    let c = Counter { n: Atomic.new(0) };
    let g = TaskGroup.new();
    let mut hs = Vec.new();
    for k in 1..9 {
        let d = c.clone();
        hs.push(g.spawn(|| work(d, k)));
    }
    let mut t = 0;
    for h in hs.into_iter() {
        t += h.join();
    }
    println(f"{c.n.load()} {t}");
}
"#;

const MUTEX: &str = r#"import std.time.{sleep, Duration};

fn bump(m: Mutex[i64], n: i64) {
    let mut i = 0;
    while i < n {
        lock m x {
            x = x + 1;
        }
        i += 1;
    }
}

fn slow(m: Mutex[i64]) {
    lock m x {
        let v = x;
        sleep(Duration.ms(1));
        x = v + 1000;
    }
}

fn main() {
    let m = Mutex.new(0);
    {
        let g = TaskGroup.new();
        for _ in 0..8 {
            g.spawn(|| bump(m, 2000));
            g.spawn(|| slow(m));
        }
    }
    lock m x { println(x); }
}
"#;

const CHANNEL: &str = r#"fn produce(tx: Sender[i64], n: i64) {
    let mut i = 1;
    while i <= n {
        tx.send(i).unwrap();
        i += 1;
    }
}

fn main() {
    let (tx, rx) = Channel.new();
    let g = TaskGroup.new();
    for _ in 0..4 {
        let t = tx.clone();
        g.spawn(|| produce(t, 500));
    }
    let mut s = 0;
    let mut k = 0;
    while k < 2000 {
        s += rx.recv().unwrap();
        k += 1;
    }
    println(s);
}
"#;

const FIB: &str = r#"fn fib(n: i64) -> i64 {
    if n < 2 {
        return n;
    }
    if n < 14 {
        return fib(n - 1) + fib(n - 2);
    }
    let g = TaskGroup.new();
    let a = g.spawn(|| fib(n - 1));
    let b = g.spawn(|| fib(n - 2));
    a.join() + b.join()
}

fn main() {
    println(fib(22));
}
"#;

/// Eight tasks count to 20000 each on one `Atomic` while they compute:
/// no increment is lost however many workers run them.
#[test]
fn mir_pool_atomic_counts_from_every_worker() {
    assert_eq!(same_on_the_pool("atomic", ATOMIC), "160000 279992\n");
}

/// Sixteen tasks take one `Mutex` thousands of times, some holding it
/// across a sleep, so the release can run on another worker than the
/// acquire: every update lands.
#[test]
fn mir_pool_mutex_excludes_across_workers() {
    assert_eq!(same_on_the_pool("mutex", MUTEX), "24000\n");
}

/// Four producers on cloned senders and the root task receiving: every
/// value arrives once.
#[test]
fn mir_pool_channel_carries_every_value() {
    assert_eq!(same_on_the_pool("chan", CHANNEL), "501000\n");
}

/// A tree of task groups, each spawning and joining two more: the queue
/// takes tasks from every worker.
#[test]
fn mir_pool_nested_groups_spawn_and_join() {
    assert_eq!(same_on_the_pool("fib", FIB), "17711\n");
}

const DEADLOCK: &str = r#"fn wait_for(rx: Receiver[i64]) -> i64 {
    rx.recv().unwrap()
}

fn main() {
    let (tx, rx) = Channel.new();
    let g = TaskGroup.new();
    let h = g.spawn(|| wait_for(rx));
    println(h.join());
    tx.send(1).unwrap();
}
"#;

/// The only task waits on a channel the root, which joins it, would send to
/// later: on every worker count the program reports the deadlock rather than
/// hang.
#[test]
fn mir_pool_reports_a_deadlock() {
    let Some((dir, exe)) = built("deadlock", DEADLOCK) else {
        return;
    };
    for threads in THREADS {
        let mut cmd = Command::new(&exe);
        cmd.env("KARAC_THREADS", threads);
        let out = run("deadlock", cmd);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(101),
            "KARAC_THREADS={threads}: {err}"
        );
        assert!(err.contains("deadlock"), "KARAC_THREADS={threads}: {err}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
