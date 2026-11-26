//! A panic's place in a REPL session names the cell and the cell's own
//! line, as a diagnostic does, both where the session reports an uncaught
//! panic and in the `location` a program reads from `TaskFailed.Panicked`:
//! not the line of the program the session runs the cell as.

use super::session;

#[test]
fn repl_panic_place_names_the_cell_of_an_item() {
    let mut s = session();
    for cell in [
        "fn g() -> i64 { 1 }",
        "let a = 1;",
        "fn f(o: Option[i64]) -> i64 {\n    o.unwrap()\n}",
    ] {
        let r = s.evaluate_cell_captured(cell);
        assert!(r.errors.is_empty(), "{cell}: {:?}", r.errors);
    }
    let r = s.evaluate_cell_captured("println(f(None));");
    let all = r.errors.join("\n");
    assert!(all.contains("cell 3:2:5"), "{all}");
}

#[test]
fn repl_panic_place_reaches_a_task_failed_location() {
    let mut s = session();
    let out: Vec<String> = [
        "import std.sync.{TaskFailed, TaskGroup};",
        "fn f(o: Option[i64]) -> i64 {\n    o.unwrap()\n}",
        "let g = TaskGroup.isolating();",
        "let h = g.spawn(|| f(None));",
        "match h.join() {\n    Err(TaskFailed.Panicked { message, location }) => println(f\"{location}: {message}\"),\n    _ => println(\"no panic\"),\n}",
    ]
    .iter()
    .map(|cell| {
        let r = s.evaluate_cell_captured(cell);
        assert!(r.errors.is_empty(), "{cell}: {:?}", r.errors);
        r.stdout
    })
    .collect();
    assert_eq!(out[4], "cell 2:2:5: called unwrap() on None\n", "{out:?}");
}
