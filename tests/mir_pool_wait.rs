//! A full `Pool`'s `acquire` waits up to its timeout for a value to come
//! back, and answers `Busy` at once when `max_waiters` callers already
//! wait (docs/library/pool.md, B-2026-10-21-44). Each program runs on both
//! MIR backends under the coroutine transform, as `tests/mir_tasks.rs`
//! does.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// How long a program may run before it counts as hung.
const DEADLINE: Duration = Duration::from_secs(20);

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
        "karac-mir-pool-wait-{tag}-{name}-{}",
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

/// The program's stdout on the interpreter and, when the runtime archive
/// links, on the built binary, which must print the same.
fn stdout_on_both(name: &str, src: &str) -> String {
    let (dir, file) = scratch(name, "i", src);
    let out = run(name, karac(&file, &["__mir-run"]));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{name} on the interpreter: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let interp = String::from_utf8(out.stdout).unwrap();

    let (dir, file) = scratch(name, "b", src);
    let exe = dir.join("main");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_MIR_COROUTINES", "1")
        .args(["build", "--mir", file.to_str().unwrap(), "-o"])
        .arg(&exe)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr).to_string();
    assert!(
        built.status.success() || err.contains("Linker failed"),
        "{name} does not build: {err}"
    );
    let linked = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(err)
    });
    if linked.is_some() {
        let out = run(name, Command::new(&exe));
        assert!(
            out.status.success(),
            "{name} built: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            interp,
            "{name}: the build and the interpreter differ"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    interp
}

const PRELUDE: &str = r#"
import std.pool.{Pool, PoolError};
import std.time.{Duration, Instant};
struct Conn { id: i64 }
fn conn(id: i64) -> Result[Conn, String] { Ok(Conn { id }) }
fn show(r: Result[i64, PoolError[String]]) -> String {
    match r {
        Ok(id) => f"got {id}",
        Err(PoolError.Timeout) => "timeout".to_string(),
        Err(PoolError.Busy) => "busy".to_string(),
        Err(PoolError.PoolClosed) => "closed".to_string(),
        Err(PoolError.CreateFailed(e)) => f"create failed {e}",
    }
}
fn take(pool: ref Pool[Conn, String], ms: i64) -> Result[i64, PoolError[String]] {
    match pool.acquire(Duration.ms(ms)) {
        Ok(c) => Ok(c.val.id),
        Err(e) => Err(e),
    }
}
"#;

/// A waiting `acquire` gets the value the holder gives back.
#[test]
fn mir_pool_acquire_waits_for_a_value_to_come_back() {
    let src = format!(
        "{PRELUDE}{}",
        r#"
fn waiter(pool: own Pool[Conn, String]) {
    println(f"waiter {show(take(pool, 5000))}");
}
fn main() {
    let pool: Pool[Conn, String] = Pool.new(|| conn(7), 1, 4);
    let group = TaskGroup.new();
    let w = {
        let held = pool.acquire(Duration.ms(10)).unwrap();
        let w = group.spawn(|| waiter(pool));
        sleep_ms(100);
        println(f"giving back {held.val.id}");
        w
    };
    w.join();
}
"#
    );
    assert_eq!(
        stdout_on_both("waits", &src),
        "giving back 7\nwaiter got 7\n"
    );
}

/// A wait that sees nothing come back ends in `Timeout`, and a pool that
/// lets no one wait answers `Busy` at once, in a task and outside one.
#[test]
fn mir_pool_acquire_times_out_or_is_busy() {
    let src = format!(
        "{PRELUDE}{}",
        r#"
fn waiter(pool: own Pool[Conn, String], ms: i64) {
    println(f"waiter {show(take(pool, ms))}");
}
fn main() {
    let pool: Pool[Conn, String] = Pool.new(|| conn(1), 1, 4);
    let held = pool.acquire(Duration.ms(10)).unwrap();
    let t0 = Instant.now();
    println(f"main {show(take(pool, 50))}");
    println(f"waited the timeout {t0.elapsed().as_ms() >= 50}");
    let group = TaskGroup.new();
    let w = group.spawn(|| waiter(pool, 50));
    w.join();
    let none: Pool[Conn, String] = Pool.new(|| conn(2), 1, 0);
    let h2 = none.acquire(Duration.ms(10)).unwrap();
    println(f"main {show(take(none, 5000))}");
    let w2 = group.spawn(|| waiter(none, 5000));
    w2.join();
    println(f"held {held.val.id} {h2.val.id}");
}
"#
    );
    assert_eq!(
        stdout_on_both("timeout", &src),
        "main timeout\nwaited the timeout true\nwaiter timeout\nmain busy\nwaiter busy\nheld 1 2\n"
    );
}

/// With `max_waiters` callers already waiting, one more is turned away
/// with `Busy` without waiting, and the waiter still gets the value.
#[test]
fn mir_pool_acquire_past_max_waiters_is_busy() {
    let src = format!(
        "{PRELUDE}{}",
        r#"
fn main() {
    let pool: Pool[Conn, String] = Pool.new(|| conn(9), 1, 1);
    let group = TaskGroup.new();
    let (a, b) = {
        let held = pool.acquire(Duration.ms(10)).unwrap();
        let a = group.spawn(|| println(f"a {show(take(pool, 5000))}"));
        let b = group.spawn(|| println(f"b {show(take(pool, 5000))}"));
        sleep_ms(100);
        println(f"giving back {held.val.id}");
        (a, b)
    };
    a.join();
    b.join();
}
"#
    );
    assert_eq!(
        stdout_on_both("busy", &src),
        "b busy\ngiving back 9\na got 9\n"
    );
}
