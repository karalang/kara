//! Coroutines as values (M4 item 4): a function that suspends, erased into
//! a function value, runs in a frame of its own on the heap when called
//! through the value, and its caller's task suspends while it waits, so the
//! other tasks run. A value of the same type that does not suspend is
//! called as before. Each program runs on both MIR backends under the
//! coroutine transform, as `tests/mir_cancel.rs` does.
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
        "karac-mir-fn-suspend-{tag}-{name}-{}",
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

/// A function item in a struct field: the call through it suspends the
/// task that makes it, and a plain value of the same field type is called
/// directly from the same call site.
#[test]
fn mir_fn_value_item_suspends_its_task() {
    let src = r#"
struct Handler {
    f: Fn(i64) -> i64 with suspends,
}

fn slow(n: i64) -> i64 {
    sleep_ms(200);
    n * 2
}

fn fast(n: i64) -> i64 {
    n + 1
}

fn ticker() -> i64 {
    for i in 0..3 {
        println(f"tick {i}");
        sleep_ms(10);
    }
    0
}

fn call(h: ref Handler, n: i64) -> i64 {
    (h.f)(n)
}

fn main() {
    let h = Handler { f: slow };
    let quick = Handler { f: fast };
    println(f"quick {call(quick, 1)}");
    let group = TaskGroup.new();
    let a = group.spawn(|| call(h, 5));
    let b = group.spawn(|| ticker());
    println(f"a {a.join()}");
    let _ = b.join();
}
"#;
    assert_eq!(
        stdout_on_both("item", src),
        "quick 2\ntick 0\ntick 1\ntick 2\na 10\n"
    );
}

/// A capturing closure an iterator adapter holds as an erased value.
#[test]
fn mir_fn_value_closure_in_an_adapter_suspends() {
    let src = r#"
fn slow(n: i64) -> i64 {
    sleep_ms(60);
    n * 2
}

fn ticker() -> i64 {
    for i in 0..3 {
        println(f"tick {i}");
        sleep_ms(50);
    }
    0
}

fn main() {
    let k = 1;
    let group = TaskGroup.new();
    let t = group.spawn(|| ticker());
    let v = [1, 2, 3];
    let w: Vec[i64] = v.iter().map(|x| slow(*x + k)).collect();
    println(f"{w}");
    let _ = t.join();
}
"#;
    assert_eq!(
        stdout_on_both("adapter", src),
        "tick 0\ntick 1\ntick 2\n[4, 6, 8]\n"
    );
}

/// A closure's frame drops its own locals when it returns, and the
/// environment it borrowed stays with the value until the value drops.
#[test]
fn mir_fn_value_closure_frame_drops_locals_and_keeps_its_environment() {
    let src = r#"
struct Noisy {
    name: String,
}

impl Drop for Noisy {
    fn drop(mut ref self) {
        println(f"drop {self.name}");
    }
}

struct Step {
    run: Fn(i64) -> String with suspends,
}

fn ticker() -> i64 {
    for i in 0..3 {
        println(f"tick {i}");
        sleep_ms(10);
    }
    0
}

fn main() {
    let tag = "slow".to_string();
    let n = Noisy { name: "env".to_string() };
    let steps = [
        Step { run: |x| {
            let local = Noisy { name: "local".to_string() };
            sleep_ms(150);
            f"{tag} {x} {n.name}"
        } },
        Step { run: |x| f"fast {x}" },
    ];
    let group = TaskGroup.new();
    let t = group.spawn(|| ticker());
    for s in steps.iter() {
        println((s.run)(7));
    }
    let _ = t.join();
}
"#;
    assert_eq!(
        stdout_on_both("closure", src),
        "tick 0\ntick 1\ntick 2\ndrop local\nslow 7 env\nfast 7\ndrop env\n"
    );
}

/// A task cancelled while it waits inside a frame called through a value
/// (the closure an adapter holds) leaves both frames by their exits,
/// innermost first.
#[test]
fn mir_fn_value_cancelled_inside_the_called_frame() {
    let src = r#"
struct Noisy {
    name: String,
}

impl Drop for Noisy {
    fn drop(mut ref self) {
        println(f"drop {self.name}");
    }
}

fn wait_for(rx: ref Receiver[String]) -> String {
    let inner = Noisy { name: "inner".to_string() };
    defer println("inner deferred");
    match rx.recv() {
        Some(s) => s,
        None => "none".to_string(),
    }
}

fn worker(rxs: own Vec[Receiver[String]]) -> i64 {
    let outer = Noisy { name: "outer".to_string() };
    defer println("outer deferred");
    let got: Vec[String] = rxs.iter().map(|rx| wait_for(rx)).collect();
    println(f"got {got}");
    1
}

fn main() {
    let (tx, rx): (Sender[String], Receiver[String]) = channel(1);
    let group = TaskGroup.new();
    let h = group.spawn(|| worker([rx]));
    sleep_ms(20);
    group.cancel();
    match h.try_join() {
        Ok(v) => println(f"ok {v}"),
        Err(_) => println("cancelled"),
    }
}
"#;
    assert_eq!(
        stdout_on_both("cancel", src),
        "inner deferred\ndrop inner\nouter deferred\ndrop outer\ncancelled\n"
    );
}

/// A panic inside a function value called from a task of an isolating
/// group ends that task alone: the value's frame and the task's locals
/// drop, its handle holds the panic, and the other task finishes.
#[test]
fn mir_fn_value_panics_in_an_isolating_task() {
    let src = r#"
struct Noisy {
    name: String,
}

impl Drop for Noisy {
    fn drop(mut ref self) {
        println(f"drop {self.name}");
    }
}

fn boom() -> i64 {
    let _g = Noisy { name: "boom".to_string() };
    panic("bad value");
}

fn fine() -> i64 {
    7
}

fn pick(n: i64) -> Fn() -> i64 {
    if n == 1 {
        boom
    } else {
        fine
    }
}

fn main() {
    let f = pick(1);
    let k = pick(2);
    let g = TaskGroup.isolating();
    let a = g.spawn(|| {
        let _h = Noisy { name: "task".to_string() };
        f()
    });
    let b = g.spawn(|| k());
    match a.join() {
        Ok(v) => println(f"a {v}"),
        Err(TaskFailed.Panicked { message, .. }) => println(f"a panicked: {message}"),
        Err(TaskFailed.Cancelled) => println("a cancelled"),
    }
    match b.join() {
        Ok(v) => println(f"b {v}"),
        Err(_) => println("b failed"),
    }
}
"#;
    assert_eq!(
        stdout_on_both("isolating", src),
        "drop boom\ndrop task\na panicked: bad value\nb 7\n"
    );
}
