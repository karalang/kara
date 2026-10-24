//! Deadlines (core-semantics §11.10, M4 item 5): a socket's read timeout
//! bounds a suspended read, which ends `TimedOut` while the program's
//! other tasks run. Each program runs on both MIR backends under the
//! coroutine transform, as `tests/mir_tasks.rs` does.
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
        "karac-mir-deadline-{tag}-{name}-{}",
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

/// The program's stdout on the interpreter.
fn stdout_on_interp(name: &str, src: &str) -> String {
    let (dir, file) = scratch(name, "i", src);
    let out = run(name, karac(&file, &["__mir-run"]));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{name} on the interpreter: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// The program's stdout on the interpreter and, when the runtime archive
/// links, on the built binary, which must print the same.
fn stdout_on_both(name: &str, src: &str) -> String {
    let interp = stdout_on_interp(name, src);

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

/// A read on a socket with a read timeout suspends its task: the other
/// task ticks while it waits, and it ends `TimedOut` at the timeout, or
/// with the bytes that arrive first. A UDP receive ends the same way.
#[test]
fn mir_deadline_read_timeout_bounds_a_suspended_read() {
    let src = r#"
fn ticks(tag: String) {
    for i in 0..3 {
        sleep_ms(20);
        println(f"{tag} tick {i}");
    }
}

fn reader(s: own TcpStream) {
    let mut buf: Array[u8, 8] = [0u8; 8];
    match s.read(mut buf) {
        Ok(n) => println(f"read {n}"),
        Err(TcpError.TimedOut) => println("read timed out"),
        Err(_) => println("read failed"),
    }
}

fn writer(c: own TcpStream) {
    sleep_ms(30);
    match c.write_all("xy".to_string().bytes()) {
        Ok(_) => println("wrote"),
        Err(_) => println("write failed"),
    }
}

fn receiver(u: own UdpSocket) {
    let mut buf: Array[u8, 8] = [0u8; 8];
    match u.recv(mut buf) {
        Ok(n) => println(f"datagram {n}"),
        Err(TcpError.TimedOut) => println("recv timed out"),
        Err(_) => println("recv failed"),
    }
}

fn main() {
    let l = TcpListener.bind("127.0.0.1:0").unwrap();
    let at = l.local_addr().unwrap();
    // Nothing arrives: the read ends at its timeout, and the other task
    // runs while it waits.
    let c = TcpStream.connect(at).unwrap();
    let server = l.accept().unwrap();
    let _ = server.set_read_timeout(Some(150));
    let group = TaskGroup.new();
    let r = group.spawn(|| reader(server));
    let t = group.spawn(|| ticks("a"));
    r.join();
    t.join();
    // Bytes arrive before the timeout: the read returns them.
    let c2 = TcpStream.connect(at).unwrap();
    let server2 = l.accept().unwrap();
    let _ = server2.set_read_timeout(Some(5000));
    let r2 = group.spawn(|| reader(server2));
    let w = group.spawn(|| writer(c2));
    r2.join();
    w.join();
    // A UDP receive ends at its timeout too.
    let u = UdpSocket.bind("127.0.0.1:0").unwrap();
    let _ = u.set_read_timeout(Some(150));
    let r3 = group.spawn(|| receiver(u));
    let t3 = group.spawn(|| ticks("b"));
    r3.join();
    t3.join();
    let _ = c;
}
"#;
    assert_eq!(
        stdout_on_both("read", src),
        "a tick 0\na tick 1\na tick 2\nread timed out\nwrote\nread 2\n\
         b tick 0\nb tick 1\nb tick 2\nrecv timed out\n"
    );
}

/// `TaskGroup.cancel` tears each unfinished task down at the wait it is
/// suspended in (core-semantics §11.9): its drops run then, a task that
/// returned keeps its value, and `try_join` tells a cancelled task by
/// `Err(TaskFailed.Cancelled)`.
#[test]
fn mir_deadline_cancel_tears_down_suspended_tasks() {
    let src = r#"
struct Noisy {
    name: String,
}

impl Drop for Noisy {
    fn drop(mut ref self) {
        println(f"drop {self.name}");
    }
}

fn waiter(rx: own Receiver[i64]) -> i64 {
    let n = Noisy { name: "waiter".to_string() };
    match rx.recv() {
        Some(v) => v,
        None => -1,
    }
}

fn sleeper() -> i64 {
    let n = Noisy { name: "sleeper".to_string() };
    sleep_ms(10000);
    println("woke");
    1
}

fn quick() -> i64 {
    7
}

fn main() {
    let (tx, rx) = channel(1);
    let group = TaskGroup.new();
    let a = group.spawn(|| waiter(rx));
    let b = group.spawn(|| sleeper());
    let c = group.spawn(|| quick());
    sleep_ms(20);
    println("cancelling");
    group.cancel();
    sleep_ms(5);
    println("after");
    match c.try_join() {
        Ok(v) => println(f"quick {v}"),
        Err(_) => println("quick failed"),
    }
    match a.try_join() {
        Ok(v) => println(f"waiter {v}"),
        Err(TaskFailed.Cancelled) => println("waiter cancelled"),
        Err(TaskFailed.Panicked { message, location }) => println(f"waiter panicked {message}"),
    }
    match b.try_join() {
        Ok(v) => println(f"sleeper {v}"),
        Err(TaskFailed.Cancelled) => println("sleeper cancelled"),
        Err(_) => println("sleeper failed"),
    }
    let _ = tx;
    println("done");
}
"#;
    assert_eq!(
        stdout_on_both("cancel", src),
        "cancelling\ndrop waiter\ndrop sleeper\nafter\nquick 7\nwaiter cancelled\nsleeper cancelled\ndone\n"
    );
}

/// Cancelling a task cancels the groups it made, innermost first: the
/// inner task's drops and `defer`s run before its owner's.
#[test]
fn mir_deadline_cancel_reaches_nested_groups() {
    let src = r#"
struct Noisy {
    name: String,
}

impl Drop for Noisy {
    fn drop(mut ref self) {
        println(f"drop {self.name}");
    }
}

fn sleeper() -> i64 {
    defer println("deferred");
    let n = Noisy { name: "inner".to_string() };
    sleep_ms(10000);
    1
}

fn nested() -> i64 {
    let n = Noisy { name: "outer".to_string() };
    let inner = TaskGroup.new();
    let h = inner.spawn(|| sleeper());
    h.join()
}

fn main() {
    let group = TaskGroup.new();
    let a = group.spawn(|| sleeper());
    let b = group.spawn(|| nested());
    sleep_ms(20);
    group.cancel();
    println("cancelled");
}
"#;
    assert_eq!(
        stdout_on_both("nested", src),
        "drop inner\ndeferred\ndrop inner\ndeferred\ndrop outer\ncancelled\n"
    );
}

/// `with_deadline` cancels the group once its deadline passes (§11.10):
/// a task that finished first keeps its value.
#[test]
fn mir_deadline_with_deadline_cancels_the_group() {
    let src = r#"
import std.time.{Duration, timeout, TimedOut};

struct Noisy {
    name: String,
}

impl Drop for Noisy {
    fn drop(mut ref self) {
        println(f"drop {self.name}");
    }
}

fn slow(ms: i64) -> i64 {
    let n = Noisy { name: f"slow {ms}" };
    sleep_ms(ms);
    ms
}

fn main() {
    let (tx, rx): (Sender[i64], Receiver[i64]) = channel(1);
    let group = TaskGroup.new().with_deadline(Duration.ms(30));
    let a = group.spawn(|| match rx.recv() { Some(v) => v, None => -1 });
    let b = group.spawn(|| slow(5));
    match b.try_join() {
        Ok(v) => println(f"b {v}"),
        Err(_) => println("b cancelled"),
    }
    match a.try_join() {
        Ok(v) => println(f"a {v}"),
        Err(TaskFailed.Cancelled) => println("a cancelled"),
        Err(_) => println("a failed"),
    }
    let _ = tx;
}
"#;
    assert_eq!(
        stdout_on_both("with_deadline", src),
        "drop slow 5\nb 5\na cancelled\n"
    );
}
