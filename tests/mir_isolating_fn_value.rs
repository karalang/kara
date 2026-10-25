//! An isolating group's `spawn` of a function value rather than a closure
//! written at the call (core-semantics §11.12, B-2026-10-25-22): a named
//! function, an associated function, a closure held in a variable and a
//! function parameter each run as the task, and a panic in them is the
//! `Err` their handle holds, as for a closure.

use karac::mir::interp::Outcome;

fn run(src: &str) -> (String, String, i32) {
    let r = karac::mir::lower::run_source(src).unwrap_or_else(|e| panic!("build: {e}"));
    match (&r.outcome, r.exit_code()) {
        (_, Some(code)) => (r.output, r.stderr, code),
        (Outcome::Error(e), None) => panic!("run: {e}\n--- output so far\n{}", r.output),
        _ => panic!("no exit code"),
    }
}

#[test]
fn mir_isolating_spawn_of_a_function_value() {
    let src = r#"
struct Job { n: i64 }

impl Job {
    fn start() -> i64 { 5 }
}

fn work() -> i64 { 7 }

fn boom() -> i64 {
    let v: Vec[i64] = Vec.new();
    v[3]
}

fn show(name: String, r: Result[i64, TaskFailed]) {
    match r {
        Ok(v) => println(f"{name} ok {v}"),
        Err(TaskFailed.Panicked { message, location }) => println(f"{name} panicked: {message}"),
        Err(TaskFailed.Cancelled) => println(f"{name} cancelled"),
    }
}

fn through(f: own OnceFn() -> i64) -> Result[i64, TaskFailed] {
    let g = TaskGroup.isolating();
    let h = g.spawn(f);
    h.join()
}

fn main() {
    let s = "moved".to_string();
    let c = || s.len();
    let g = TaskGroup.isolating();
    let a = g.spawn(work);
    let b = g.spawn(boom);
    let j = g.spawn(Job.start);
    let k = g.spawn(c);
    show("work", a.join());
    show("boom", b.join());
    show("start", j.join());
    show("closure", k.join());
    show("param work", through(work));
    show("param boom", through(boom));
    show("param closure", through(|| 41 + 1));
}
"#;
    let (out, err, code) = run(src);
    assert_eq!(
        (out.as_str(), code),
        (
            "work ok 7\nboom panicked: index out of bounds\nstart ok 5\nclosure ok 5\n\
             param work ok 7\nparam boom panicked: index out of bounds\nparam closure ok 42\n",
            0
        ),
        "stderr: {err}"
    );
}
