//! B-2026-11-19-112: an E0516 whose borrow a `TaskGroup`'s task
//! holds advised `.clone()` "where it is borrowed", which does not end the
//! borrow: a task keeps what it borrows until its group drops and joins it
//! (core semantics §9.5). The help now says so, and to end the group in an
//! inner block first (kata 1242).

use std::process::{Command, Stdio};

#[test]
fn e0516_held_by_a_task_group_says_the_group_must_end_first() {
    let src = "import std.sync.{TaskGroup};
fn consume(v: own Vec[i64]) -> i64 { v.len() }
fn main() {
    let xs: Vec[i64] = [1, 2, 3];
    let group = TaskGroup.new();
    group.spawn(|| println(f\"{xs.len()}\"));
    println(consume(xs));
}
";
    let path = std::env::temp_dir().join(format!("karac-e0516-group-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{said}");
    assert!(said.contains("[E0516]"), "{said}");
    assert!(
        said.contains(
            "A task spawned into `group` keeps the borrow until `group` drops, which joins its \
             tasks (core-semantics.md §9.5): put `group` in an inner block"
        ),
        "{said}"
    );
    assert!(!said.contains("`*r` for a `Copy` value"), "{said}");
}
