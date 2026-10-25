//! Isolating task groups on the MIR interpreter (core-semantics §11.12):
//! a panic in a task of `TaskGroup.isolating()` ends that task, torn down
//! as a cancelled task is, and its handle holds `Err(Panicked { .. })`;
//! the group's other tasks run on. Everywhere else a panic still ends the
//! process. The compiled backend's half is the drop schedule's.

use karac::mir::interp::Outcome;

fn run(src: &str) -> (String, String, i32) {
    let r = karac::mir::lower::run_source(src).unwrap_or_else(|e| panic!("build: {e}"));
    match (&r.outcome, r.exit_code()) {
        (_, Some(code)) => (r.output, r.stderr, code),
        (Outcome::Error(e), None) => panic!("run: {e}\n--- output so far\n{}", r.output),
        _ => panic!("no exit code"),
    }
}

fn assert_runs(src: &str, want: &str) {
    let (out, err, code) = run(src);
    assert_eq!((out.as_str(), code), (want, 0), "stderr: {err}");
}

/// The task that panics runs its drops and `defer`s on the way out, its
/// handle holds the panic, and the tasks beside it finish.
#[test]
fn mir_isolating_a_panic_ends_its_task_alone() {
    assert_runs(
        r#"
struct Noisy { name: String }

impl Drop for Noisy {
    fn drop(mut ref self) {
        println(f"drop {self.name}");
    }
}

fn risky(n: i64) -> i64 {
    let _g = Noisy { name: f"risky {n}" };
    if n == 2 {
        panic("bad request");
    }
    n * 10
}

fn show(name: String, r: Result[i64, TaskFailed]) {
    match r {
        Ok(v) => println(f"{name} {v}"),
        Err(TaskFailed.Panicked { message, location }) => {
            let at = location.contains(":");
            println(f"{name} panicked: {message} (located {at})");
        }
        Err(TaskFailed.Cancelled) => println(f"{name} cancelled"),
    }
}

fn main() {
    let g = TaskGroup.isolating();
    let a = g.spawn(|| risky(1));
    let b = g.spawn(|| {
        defer println("defer in b");
        let _h = Noisy { name: "b's own" };
        risky(2)
    });
    let c = g.spawn(|| risky(3));
    show("a", a.join());
    show("b", b.join());
    show("c", c.join());
    println("main goes on");
}
"#,
        "drop risky 1\ndrop risky 2\ndrop b's own\ndefer in b\ndrop risky 3\na 10\n\
         b panicked: bad request (located true)\nc 30\nmain goes on\n",
    );
}

/// A panic in a library body, an overflow, an index out of bounds and a
/// panic in a closure a native calls are each contained, with nothing
/// leaked; `?` and `return` in the task leave with its own value, in `Ok`.
#[test]
fn mir_isolating_every_kind_of_panic_is_contained() {
    assert_runs(
        r#"
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
"#,
        "drop r\nq err not a number\nr ok 7\nu panicked: called unwrap() on None\n\
         o panicked: integer overflow\ni panicked: index out of bounds\nm panicked: no twos\n\
         t 5\ndone\n",
    );
}

/// The teardown runs plain `errdefer`s and skips `errdefer(e)`, as a
/// cancel's does; a task suspended on a channel goes on; a cancelled task
/// of an isolating group is `Err(Cancelled)` from `join`.
#[test]
fn mir_isolating_teardown_and_cancellation() {
    assert_runs(
        r#"
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
"#,
        "defer 1\nerrdefer 1\ndefer 2\np panicked: step one\ns 2\nw 42\nk cancelled\ndone\n",
    );
    assert_runs(
        r#"
fn main() {
    let (tx, rx): (Sender[i64], Receiver[i64]) = channel(1);
    let g = TaskGroup.isolating();
    let waiter = g.spawn(|| match rx.recv() { Some(v) => v, None => -1 });
    let bad = g.spawn(|| {
        let v: Vec[i64] = [];
        v[0]
    });
    match bad.join() {
        Err(TaskFailed.Panicked { message, .. }) => println(f"bad: {message}"),
        _ => println("bad other"),
    }
    g.cancel();
    match waiter.join() {
        Err(TaskFailed.Cancelled) => println("waiter cancelled"),
        _ => println("waiter other"),
    }
}
"#,
        "bad: index out of bounds\nwaiter cancelled\n",
    );
}

/// A task whose handle is dropped still runs, and its panic goes nowhere.
#[test]
fn mir_isolating_a_dropped_handle_takes_the_panic_with_it() {
    assert_runs(
        r#"
fn main() {
    let g = TaskGroup.isolating();
    g.spawn(|| {
        println("one");
        panic("dropped handle");
    });
    g.spawn(|| println("two"));
    println("main");
}
"#,
        "main\none\ntwo\n",
    );
}

/// In a program with an isolating group, a panic in `main`, in a task of
/// an ordinary group inside an isolating task, or in a `par` branch inside
/// one still ends the process (§11.12, §10.2), reported as before.
#[test]
fn mir_isolating_a_panic_elsewhere_still_ends_the_process() {
    let cases = [
        (
            r#"
fn main() {
    let g = TaskGroup.isolating();
    let h = g.spawn(|| 1);
    println(f"{h.join().is_ok()}");
    let v: Vec[i64] = [1, 2];
    println(f"{v[5]}");
}
"#,
            "true\n",
            "runtime error: index out of bounds\n  at <unknown>:7:16\n",
        ),
        (
            r#"
fn main() {
    let g = TaskGroup.isolating();
    let h = g.spawn(|| {
        let inner = TaskGroup.new();
        let k = inner.spawn(|| {
            let v: Vec[i64] = [1];
            v[2]
        });
        k.join()
    });
    println(f"{h.join().is_ok()}");
}
"#,
            "",
            "runtime error: index out of bounds\n  at <unknown>:8:13\n",
        ),
        (
            r#"
fn main() {
    let g = TaskGroup.isolating();
    let h = g.spawn(|| {
        let (a, b) = par {
            1,
            panic("in a branch"),
        };
        a
    });
    println(f"{h.join().is_ok()}");
}
"#,
            "",
            "runtime error: in a branch\n  at <unknown>:7:13\n",
        ),
    ];
    for (src, want, report) in cases {
        let (out, err, code) = run(src);
        assert_eq!((out.as_str(), code), (want, 101), "stderr: {err}");
        assert!(err.starts_with(report), "{err}");
    }
}
