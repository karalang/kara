//! Isolating groups in built binaries (core-semantics §11.12, M4 item 9's
//! compiled half): a panic in a task of an isolating group ends that task
//! by its exits, its handle holds `Err(Panicked)`, and the group's other
//! tasks run on. Each program prints the same on the interpreter and built
//! (`tests/mir_isolating.rs` holds the interpreter's own pins).
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
        "karac-mir-isolating-built-{tag}-{name}-{}",
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
    stdout_on_both_env(name, src, &[])
}

/// [`stdout_on_both`], with `env` set for the built binary's run.
fn stdout_on_both_env(name: &str, src: &str, env: &[(&str, &str)]) -> String {
    let interp = stdout_on_interp(name, src);

    // The same path as the interpreter's run, so a panic's location reads
    // the same.
    let (dir, file) = scratch(name, "i", src);
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
        let mut cmd = Command::new(&exe);
        cmd.envs(env.iter().copied());
        let out = run(name, cmd);
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

/// `tests/mir_isolating.rs`'s every-kind program, built: a library
/// body's panic, an overflow, an index out of bounds and a panic in a
/// closure `sort_by` calls are each contained.
#[test]
fn mir_isolating_built_contains_every_kind_of_panic() {
    let src = r#"
struct Noisy { name: String }

impl Drop for Noisy {
    fn drop(mut ref self) {
        println(f"drop {self.name}");
    }
}

fn parse(s: String) -> Result[i64, String] {
    if s == "x" {
        return Err("not a number");
    }
    Ok(s.len())
}

fn first_big(v: ref Vec[i64]) -> i64 {
    let mut big: Option[i64] = None;
    for x in v {
        if x > 100 {
            big = Some(x);
        }
    }
    big.unwrap()
}

fn show[T](name: String, r: Result[T, TaskFailed]) with writes(Stdout) where T: Display {
    match r {
        Ok(v) => println(f"{name} ok {v}"),
        Err(TaskFailed.Panicked { message, location: _ }) => println(f"{name} panicked: {message}"),
        Err(TaskFailed.Cancelled) => println(f"{name} cancelled"),
    }
}

fn main() {
    let words: Vec[String] = ["ab", "x"];
    let g = TaskGroup.isolating(limit: 2);
    // `?` and `return` leave the closure with its own value, in `Ok`.
    let q = g.spawn(|| {
        let n = parse(words[1].clone())?;
        let m: Result[i64, String] = Ok(n);
        m
    });
    let r = g.spawn(|| {
        let _a = Noisy { name: "r" };
        if words.len() > 1 {
            return 7;
        }
        8
    });
    // A panic in a library body, an overflow and an index out of bounds.
    let u = g.spawn(|| first_big([1, 2, 3]));
    let o = g.spawn(|| {
        let mut x: i64 = 9223372036854775807;
        x = x + words.len();
        x
    });
    let i = g.spawn(|| {
        let v: Vec[i64] = [1];
        v[3]
    });
    // A panic in a closure a library native calls.
    let m = g.spawn(|| {
        let mut v: Vec[i64] = [3, 1, 2];
        v.sort_by(|a, b| {
            if a == 2 {
                panic("no twos");
            }
            a.cmp(b)
        });
        v.len()
    });
    match q.join() {
        Ok(Err(e)) => println(f"q err {e}"),
        Ok(Ok(n)) => println(f"q {n}"),
        Err(_) => println("q failed"),
    }
    show("r", r.join());
    show("u", u.join());
    show("o", o.join());
    show("i", i.join());
    show("m", m.join());
    let t = g.spawn(|| 5);
    match t.try_join() {
        Ok(Ok(v)) => println(f"t {v}"),
        _ => println("t failed"),
    }
    println("done");
}
"#;
    assert_eq!(
        stdout_on_both("every", src),
        "drop r\nq err not a number\nr ok 7\nu panicked: called unwrap() on None\n\
         o panicked: integer overflow\ni panicked: index out of bounds\nm panicked: no twos\n\
         t 5\ndone\n"
    );
}

/// A panic inside a native's work ends the native there: in a comparator
/// `sort_by` calls, in a predicate `retain` calls, and in a library body
/// a task calls; the task's locals drop, and the location is the panic's
/// own place in the program.
#[test]
fn mir_isolating_built_ends_the_native_a_panic_is_in() {
    let src = r#"struct Noisy {
    name: String,
}

impl Drop for Noisy {
    fn drop(mut ref self) {
        println(f"drop {self.name}");
    }
}

fn show(name: String, r: Result[i64, TaskFailed]) {
    match r {
        Ok(v) => println(f"{name} ok {v}"),
        Err(TaskFailed.Panicked { message, location }) => println(f"{name} panicked: {message} at {location}"),
        Err(TaskFailed.Cancelled) => println(f"{name} cancelled"),
    }
}

fn main() {
    let g = TaskGroup.isolating();
    let a = g.spawn(|| {
        let _n = Noisy { name: "sort".to_string() };
        let mut v: Vec[i64] = [3, 1, 2, 5, 4];
        v.sort_by(|x, y| {
            if x == 2 {
                panic("cmp two");
            }
            x.cmp(y)
        });
        v.len() as i64
    });
    let b = g.spawn(|| {
        let _n = Noisy { name: "retain".to_string() };
        let mut v: Vec[i64] = [1, 2, 3];
        v.retain(|x| {
            if x == 2 {
                panic("retain two");
            }
            true
        });
        v.len() as i64
    });
    let c = g.spawn(|| {
        let s = "abc".to_string();
        let n: i64 = i64.parse(s).unwrap();
        n
    });
    let d = g.spawn(|| {
        let v: Vec[i64] = [1, 2, 3];
        let total: i64 = v.iter().map(|x| if x == 3 { panic("three") } else { x }).sum();
        total
    });
    show("a".to_string(), a.join());
    show("b".to_string(), b.join());
    match c.join() {
        Err(TaskFailed.Panicked { message, .. }) => println(f"c panicked: {message}"),
        _ => println("c other"),
    }
    show("d".to_string(), d.join());
}
"#;
    // One worker: tasks `a` and `b` each print as they end, and on the
    // pool which of them ends first is unspecified (core-semantics §11.1).
    let out = stdout_on_both_env("natives", src, &[("KARAC_THREADS", "1")]);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[..2], ["drop sort", "drop retain"], "{out}");
    assert!(
        lines[2].starts_with("a panicked: cmp two at ") && lines[2].ends_with("main.kara:26:17"),
        "{out}"
    );
    assert!(
        lines[3].starts_with("b panicked: retain two at ") && lines[3].ends_with("main.kara:37:17"),
        "{out}"
    );
    assert_eq!(lines[4], "c panicked: called unwrap() on Err", "{out}");
    assert!(
        lines[5].starts_with("d panicked: three at ") && lines[5].ends_with("main.kara:50:55"),
        "{out}"
    );
}

/// Division by zero, overflow, a missing map key and a `split_off` past
/// the end each end their task alone.
#[test]
fn mir_isolating_built_contains_runtime_errors() {
    let src = r#"fn show(name: String, r: Result[i64, TaskFailed]) {
    match r {
        Ok(v) => println(f"{name} ok {v}"),
        Err(TaskFailed.Panicked { message, .. }) => println(f"{name} panicked: {message}"),
        Err(TaskFailed.Cancelled) => println(f"{name} cancelled"),
    }
}

fn div(a: i64, b: i64) -> i64 {
    a / b
}

fn main() {
    let g = TaskGroup.isolating();
    let a = g.spawn(|| div(1, 0));
    let b = g.spawn(|| {
        let s = "héllo".to_string();
        let t = s.slice(0, 2);
        t.len() as i64
    });
    let c = g.spawn(|| {
        let m: Map[String, i64] = Map.new();
        m["missing"]
    });
    let d = g.spawn(|| {
        let v: Vec[i64] = [1, 2, 3];
        let w = v.split_off(9);
        w.len() as i64
    });
    let e = g.spawn(|| {
        let x: i64 = 9223372036854775807;
        x * 2
    });
    let f = g.spawn(|| {
        let s = "abc".to_string();
        let ch = s.char_at(10);
        1
    });
    show("a".to_string(), a.join());
    show("b".to_string(), b.join());
    show("c".to_string(), c.join());
    show("d".to_string(), d.join());
    show("e".to_string(), e.join());
    show("f".to_string(), f.join());
    println("done");
}
"#;
    assert_eq!(
        stdout_on_both("errors", src),
        "a panicked: division by zero\nb ok 4\nc panicked: key not found\n\
         d panicked: index out of bounds\ne panicked: integer overflow\nf ok 1\ndone\n"
    );
}

/// The teardown of a panicked task runs its `defer`s and plain
/// `errdefer`s; a sibling waiting on a channel runs on; a task cancelled
/// by the group's deadline is `Err(Cancelled)`.
#[test]
fn mir_isolating_built_tears_down_and_cancels() {
    let src = r#"
import std.time.{Duration};

fn step(n: i64) -> Result[i64, String] {
    errdefer println(f"errdefer {n}");
    errdefer(e) println(f"errdefer(e) {n} {e}");
    defer println(f"defer {n}");
    if n == 1 {
        panic("step one");
    }
    Ok(n)
}

fn main() {
    let (tx, rx): (Sender[i64], Receiver[i64]) = channel(1);
    let g = TaskGroup.isolating();
    // A task waiting on the channel runs on after its sibling panics.
    let w = g.spawn(|| {
        let v = rx.recv();
        v.unwrap_or(0) + 1
    });
    let p = g.spawn(|| step(1));
    let s = g.spawn(|| {
        let _ = tx.send(41);
        step(2)
    });
    match p.try_join() {
        Ok(Err(TaskFailed.Panicked { message, location: _ })) => println(f"p panicked: {message}"),
        _ => println("p other"),
    }
    match s.join() {
        Ok(Ok(v)) => println(f"s {v}"),
        _ => println("s other"),
    }
    match w.join() {
        Ok(v) => println(f"w {v}"),
        Err(_) => println("w failed"),
    }
    // A cancelled task of an isolating group is `Cancelled`.
    let (tx2, rx2): (Sender[i64], Receiver[i64]) = channel(1);
    let c = TaskGroup.isolating().with_deadline(Duration.ms(20));
    let k = c.spawn(|| rx2.recv().unwrap_or(0));
    match k.join() {
        Err(TaskFailed.Cancelled) => println("k cancelled"),
        Err(TaskFailed.Panicked { .. }) => println("k panicked"),
        Ok(v) => println(f"k {v}"),
    }
    let _ = tx2.send(1);
    println("done");
}
"#;
    // One worker: `p` and `s` print their defers as they run, and on the
    // pool their lines interleave in an order §11.1 leaves open.
    assert_eq!(
        stdout_on_both_env("teardown", src, &[("KARAC_THREADS", "1")]),
        "defer 1\nerrdefer 1\ndefer 2\np panicked: step one\ns 2\nw 42\nk cancelled\ndone\n"
    );
}
