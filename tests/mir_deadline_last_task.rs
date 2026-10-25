//! A group's deadline on the MIR interpreter (core-semantics §11.10).

use karac::mir::interp::Outcome;

/// The deadline fires between tasks, so the task that ran last is torn
/// down at its wait like the rest; before, it was taken for the running
/// task, marked cancelled and left parked, and the run reported a
/// deadlock (B-2026-10-25-21).
#[test]
fn mir_deadline_tears_down_the_task_that_ran_last() {
    let src = r#"
import std.time.{Duration};

fn main() {
    let (tx2, rx2): (Sender[i64], Receiver[i64]) = channel(1);
    let c = TaskGroup.new().with_deadline(Duration.ms(20));
    let k = c.spawn(|| match rx2.recv() { Some(v) => v, None => -1 });
    match k.try_join() {
        Err(TaskFailed.Cancelled) => println("k cancelled"),
        Err(TaskFailed.Panicked { .. }) => println("k panicked"),
        Ok(v) => println(f"k {v}"),
    }
    let _ = tx2.send(1);
    println("done");
}
"#;
    let r = karac::mir::lower::run_source(src).unwrap_or_else(|e| panic!("build: {e}"));
    if let Outcome::Error(e) = &r.outcome {
        panic!("run: {e}\n--- output so far\n{}", r.output);
    }
    assert_eq!(
        (r.output.as_str(), r.exit_code()),
        ("k cancelled\ndone\n", Some(0))
    );
}
