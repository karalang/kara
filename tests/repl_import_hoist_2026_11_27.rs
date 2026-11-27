//! A REPL cell pasted with its imports (`import std.sync.{TaskGroup};`
//! above statements) runs: the imports join the session's top level as a
//! file's do, and the statements run as a cell. Before, the cell was
//! refused as an import inside the synthetic `fn main()`.

use karac::repl::{ReplOptions, Session};

fn session() -> Session {
    Session::with_options(ReplOptions::default())
}

#[test]
fn a_mixed_cell_hoists_its_imports_and_runs_its_statements() {
    let mut s = session();
    let r = s.evaluate_cell_captured(
        "import std.sync.{TaskGroup};\nlet g = TaskGroup.new();\nlet h = g.spawn(|| 41);\nprintln(h.join() + 1);",
    );
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout, "42\n");
    // The import is the session's now: a later cell names it bare.
    let r = s.evaluate_cell_captured("let g2 = TaskGroup.new();\nprintln(g2.spawn(|| 7).join());");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout, "7\n");
}

#[test]
fn an_import_on_the_statements_line_is_hoisted_too() {
    let mut s = session();
    let r = s.evaluate_cell_captured(
        "import std.sync.{Semaphore}; let sem = Semaphore.new(2); println(\"ok\")",
    );
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout, "ok\n");
}

#[test]
fn a_hoisted_cell_keeps_its_lines_and_a_bad_import_refuses_it() {
    let mut s = session();
    // The statements keep the lines they were typed on.
    let r = s.evaluate_cell_captured(
        "import std.sync.{TaskGroup};\nlet v: Vec[i64] = Vec.new();\nprintln(v[3]);",
    );
    assert!(
        r.errors.iter().any(|e| e.contains("cell 1:3:")),
        "{:?}",
        r.errors
    );
    // A module that does not exist refuses the cell, and the session
    // goes on.
    let r = s.evaluate_cell_captured("import std.nosuch.{X};\nprintln(1);");
    assert!(!r.errors.is_empty(), "accepted: {:?}", r.stdout);
    let r = s.evaluate_cell_captured("println(2);");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout, "2\n");
}
