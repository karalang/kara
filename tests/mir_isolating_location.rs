//! Where a task of an isolating group panicked (core-semantics §11.12)
//! when the panic is raised in a library body (`unwrap`'s `panic`): the
//! `location` of `TaskFailed.Panicked` is the program's call into the
//! library, as the report of a panic that ends the process places it, not
//! a line of the appended library (B-2026-10-25-39).

use karac::mir::interp::Outcome;

#[test]
fn mir_isolating_panic_in_a_library_body_is_placed_at_the_call() {
    let src = r#"
fn parse_it(s: String) -> i64 {
    let n = i64.parse(s).unwrap();
    n
}

fn index_it() -> i64 {
    let v: Vec[i64] = Vec.new();
    v[2]
}

fn show(r: Result[i64, TaskFailed]) {
    match r {
        Ok(v) => println(f"ok {v}"),
        Err(TaskFailed.Panicked { message, location }) => println(f"{location}: {message}"),
        Err(TaskFailed.Cancelled) => println("cancelled"),
    }
}

fn main() {
    let none: Option[i64] = None;
    let g = TaskGroup.isolating();
    let a = g.spawn(|| parse_it("x".to_string()));
    let b = g.spawn(|| index_it());
    let c = g.spawn(|| none.unwrap());
    show(a.join());
    show(b.join());
    show(c.join());
}
"#;
    let r = karac::mir::lower::run_source(src).unwrap_or_else(|e| panic!("build: {e}"));
    if let Outcome::Error(e) = &r.outcome {
        panic!("run: {e}\n--- output so far\n{}", r.output);
    }
    // `run_source` names no file.
    let out = r.output.replace("<unknown>:", "");
    assert_eq!(
        out,
        "3:13: called unwrap() on Err\n9:5: index out of bounds\n25:24: called unwrap() on None\n",
        "stderr: {}",
        r.stderr
    );
}
