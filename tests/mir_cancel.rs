//! Cancellation (core-semantics §11.9, M4 item 5): a cancelled task is torn
//! down at its wait as an error exit from every frame on its chain, its
//! drops and `defer`s run innermost first, and the call it waited in is
//! never made. Each program runs on both MIR backends under the coroutine
//! transform, as `tests/mir_deadline.rs` does.
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
        "karac-mir-cancel-{tag}-{name}-{}",
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

/// A task torn down inside a callee leaves both frames by their exits:
/// the callee's drops, then the caller's `defer` and drops, and the
/// callee's result, which is no value, is never dropped. A send that was
/// waiting drops the value it never sent, and the queued value stays.
#[test]
fn mir_cancel_exits_every_frame_and_drops_the_unsent_value() {
    let src = r#"
struct Noisy {
    name: String,
}

impl Drop for Noisy {
    fn drop(mut ref self) {
        println(f"drop {self.name}");
    }
}

fn fetch(rx: ref Receiver[String]) -> String {
    let inner = Noisy { name: "fetch".to_string() };
    match rx.recv() {
        Some(s) => s,
        None => "none".to_string(),
    }
}

fn reader(rx: own Receiver[String]) -> i64 {
    let a = Noisy { name: "reader".to_string() };
    defer println("reader deferred");
    let s = fetch(rx);
    println(f"got {s}");
    1
}

fn sender(tx: own Sender[Noisy]) -> i64 {
    let held = Noisy { name: "held".to_string() };
    tx.send(Noisy { name: "unsent".to_string() });
    println("sent");
    2
}

fn main() {
    let (tx, rx): (Sender[String], Receiver[String]) = channel(1);
    let (ntx, nrx): (Sender[Noisy], Receiver[Noisy]) = channel(1);
    let _ = ntx.send(Noisy { name: "queued".to_string() });
    let group = TaskGroup.new();
    let a = group.spawn(|| reader(rx));
    let b = group.spawn(|| sender(ntx));
    sleep_ms(20);
    println("cancelling");
    group.cancel();
    match a.try_join() {
        Ok(v) => println(f"reader {v}"),
        Err(_) => println("reader cancelled"),
    }
    match b.try_join() {
        Ok(v) => println(f"sender {v}"),
        Err(_) => println("sender cancelled"),
    }
    match nrx.recv() {
        Some(n) => println(f"left {n.name}"),
        None => println("left none"),
    }
    let _ = tx;
    println("done");
}
"#;
    assert_eq!(
        stdout_on_both("exits", src),
        "cancelling
drop fetch
reader deferred
drop reader
drop unsent
drop held
reader cancelled
sender cancelled
left queued
drop queued
done
"
    );
}

/// The running task that cancels its own group goes on to the next wait
/// it suspends in, and is torn down there. A task cancelled before it ran
/// runs to its first wait.
#[test]
fn mir_cancel_running_and_unstarted_tasks() {
    let src = r#"
struct Noisy {
    name: String,
}

impl Drop for Noisy {
    fn drop(mut ref self) {
        println(f"drop {self.name}");
    }
}

fn worker(g: ref TaskGroup) -> i64 {
    let n = Noisy { name: "worker".to_string() };
    g.cancel();
    println("still running");
    sleep_ms(1);
    println("never");
    3
}

fn eager() -> i64 {
    let n = Noisy { name: "eager".to_string() };
    println("eager started");
    sleep_ms(1000);
    4
}

fn main() {
    let group = TaskGroup.new();
    let h = group.spawn(|| worker(group));
    match h.try_join() {
        Ok(v) => println(f"worker {v}"),
        Err(_) => println("worker cancelled"),
    }
    let other = TaskGroup.new();
    let e = other.spawn(|| eager());
    other.cancel();
    println("cancelled eager");
    match e.try_join() {
        Ok(v) => println(f"eager {v}"),
        Err(_) => println("eager cancelled"),
    }
}
"#;
    assert_eq!(
        stdout_on_both("own", src),
        "still running
drop worker
worker cancelled
eager started
drop eager
cancelled eager
eager cancelled
"
    );
}
