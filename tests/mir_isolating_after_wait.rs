//! A task of an isolating group that panics after it has waited (core-
//! semantics §11.12): its body is then a coroutine polled from the task's
//! own, and the panic leaves through that poll to the task's exit, so its
//! handle holds `Panicked` and the program goes on. The MIR interpreter
//! ended the process there (found by B-2026-10-25-41's server pin, whose
//! connection tasks wait on their sockets before a handler runs).

use karac::mir::interp::Outcome;

#[test]
fn mir_isolating_panic_after_a_wait_ends_only_its_task() {
    let src = r#"
fn boom(n: i64) -> i64 {
    let none: Option[i64] = None;
    none.unwrap() + n
}

fn after_wait(n: i64) -> i64 {
    sleep_ms(1);
    boom(n)
}

fn through(f: Fn(i64) -> i64) -> i64 {
    sleep_ms(1);
    f(1)
}

fn show(r: Result[i64, TaskFailed]) {
    match r {
        Ok(v) => println(f"ok {v}"),
        Err(TaskFailed.Panicked { message, location: _ }) => println(f"panicked: {message}"),
        Err(TaskFailed.Cancelled) => println("cancelled"),
    }
}

fn main() {
    let g = TaskGroup.isolating();
    let a = g.spawn(|| after_wait(1));
    let b = g.spawn(|| through(boom));
    let c = g.spawn(|| through(|x| x + 1));
    show(a.join());
    show(b.join());
    show(c.join());
    println("still here");
}
"#;
    let r = karac::mir::lower::run_source(src).unwrap_or_else(|e| panic!("build: {e}"));
    if let Outcome::Error(e) = &r.outcome {
        panic!("run: {e}\n--- output so far\n{}", r.output);
    }
    assert_eq!(
        r.output,
        "panicked: called unwrap() on None\npanicked: called unwrap() on None\nok 2\nstill here\n",
        "stderr: {}",
        r.stderr
    );
}
