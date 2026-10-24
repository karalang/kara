//! `select` (core-semantics §11.11, M4 item 6): the source-first ready arm
//! runs and alone performs its operation, `after` and `default` arms, a
//! wait that lets other tasks run, and `recv_timeout` / `send_timeout`.
//! Each program runs on both MIR backends under the coroutine transform,
//! as `tests/mir_tasks.rs` does.
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
        "karac-mir-select-{tag}-{name}-{}",
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

/// A `select` waits for the arm another task makes ready, and the
/// first ready arm in source order wins when several are.
#[test]
fn mir_select_waits_for_another_task_and_prefers_the_first_arm() {
    let src = r#"
import std.time.{Duration};
fn worker(tx: own Sender[i64]) {
    sleep_ms(30);
    let _ = tx.send(42);
}
fn main() {
    let (tx, rx) = channel(1);
    let (dtx, drx) = channel(1);
    let a = select {
        v = rx.recv() => f"got {v.unwrap()}",
        default => "idle".to_string(),
    };
    println(a);
    let b = select {
        r = dtx.send("hello".to_string()) => f"sent {r.is_ok()}",
        _ = after(Duration.ms(1000)) => "timeout".to_string(),
    };
    println(b);
    let c = select {
        v = rx.recv() => f"got {v.unwrap()}",
        _ = after(Duration.ms(20)) => "timeout".to_string(),
    };
    println(c);
    let group = TaskGroup.new();
    let h = group.spawn(|| worker(tx));
    let d = select {
        v = rx.recv() => f"got {v.unwrap()}",
        _ = after(Duration.ms(5000)) => "timeout".to_string(),
    };
    println(d);
    h.join();
    let e = select {
        v = rx.recv() => f"closed {v.is_none()}",
        _ = after(Duration.ms(5000)) => "timeout".to_string(),
    };
    println(e);
    match drx.recv() { Some(s) => println(s), None => println("none") }
}
"#;
    assert_eq!(
        stdout_on_both("wait", src),
        "idle\nsent true\ntimeout\ngot 42\nclosed true\nhello\n"
    );
}

/// The arm bodies run outside the wait, so `break` and `continue` mean the
/// loop around the `select`; a channel given by a call is evaluated once;
/// a send arm that loses drops its value as the `select` ends; a `select`
/// nests in an arm.
#[test]
fn mir_select_in_a_loop_with_break_and_nested() {
    let src = r#"
import std.time.{Duration};
struct Noisy { n: i64 }
impl Drop for Noisy {
    fn drop(mut ref self) { println(f"drop {self.n}"); }
}
fn producer(tx: own Sender[i64], stop: own Sender[()]) {
    let mut i = 0;
    while i < 3 {
        let _ = tx.send(i);
        sleep_ms(10);
        i += 1;
    }
    let _ = stop.send(());
}
fn pick(rx: ref Receiver[i64]) -> ref Receiver[i64] {
    println("picked");
    rx
}
fn main() {
    let (tx, rx) = channel(4);
    let (stx, srx) = channel(1);
    let group = TaskGroup.new();
    let h = group.spawn(|| producer(tx, stx));
    let mut total = 0;
    let mut rounds = 0;
    loop {
        rounds += 1;
        select {
            _ = srx.recv() => { println("stop"); break }
            m = pick(rx).recv() => match m { Some(v) => total += v, None => break },
            _ = after(Duration.ms(5000)) => { println("tick"); continue }
        }
    }
    println(f"total {total} rounds {rounds}");
    h.join();
    let (ntx, nrx) = channel(1);
    let _ = ntx.send(Noisy { n: 0 });
    let r = select {
        r = ntx.send(Noisy { n: 1 }) => f"sent {r.is_ok()}",
        default => "full".to_string(),
    };
    println(r);
    let inner = select {
        v = nrx.recv() => select {
            w = nrx.recv() => f"two {w.is_some()}",
            default => f"one {v.unwrap().n}",
        },
        default => "none".to_string(),
    };
    println(inner);
}
"#;
    assert_eq!(
        stdout_on_both("loop", src),
        "picked\npicked\npicked\npicked\nstop\ntotal 3 rounds 4\ndrop 1\nfull\ndrop 0\none 0\n"
    );
}

/// `recv_timeout` and `send_timeout` behave as two-arm `select`s, and
/// their errors tell a closed channel from a timeout; a send that fails
/// hands its value back.
#[test]
fn mir_select_recv_and_send_timeout() {
    let src = r#"
import std.time.{Duration};
fn later(tx: own Sender[i64]) {
    sleep_ms(30);
    let _ = tx.send(7);
}
fn recv_text(r: Result[i64, RecvTimeoutError]) -> String {
    match r {
        Ok(v) => f"got {v}",
        Err(RecvTimeoutError.TimedOut) => "timed out".to_string(),
        Err(RecvTimeoutError.Closed) => "closed".to_string(),
    }
}
fn main() {
    let (tx, rx) = channel(1);
    println(recv_text(rx.recv_timeout(Duration.ms(10))));
    let group = TaskGroup.new();
    let h = group.spawn(|| later(tx));
    println(recv_text(rx.recv_timeout(Duration.ms(5000))));
    h.join();
    println(recv_text(rx.recv_timeout(Duration.ms(5000))));
    let (stx, srx) = channel(1);
    println(f"{stx.send_timeout("a".to_string(), Duration.ms(10)).is_ok()}");
    match stx.send_timeout("b".to_string(), Duration.ms(10)) {
        Ok(()) => println("sent b"),
        Err(SendTimeoutError.TimedOut(v)) => println(f"timed out, back {v}"),
        Err(SendTimeoutError.Closed(v)) => println(f"closed, back {v}"),
    }
    match srx.recv() { Some(s) => println(s), None => println("none") }
    {
        let gone = srx;
        let _ = gone;
    }
    match stx.send_timeout("c".to_string(), Duration.ms(5000)) {
        Ok(()) => println("sent c"),
        Err(SendTimeoutError.TimedOut(v)) => println(f"timed out, back {v}"),
        Err(SendTimeoutError.Closed(v)) => println(f"closed, back {v}"),
    }
}
"#;
    assert_eq!(
        stdout_on_both("timeout", src),
        "timed out\ngot 7\nclosed\ntrue\ntimed out, back b\na\nclosed, back c\n"
    );
}
