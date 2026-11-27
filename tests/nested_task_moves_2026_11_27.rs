//! B-2026-11-27-46: a task spawned from inside another task takes that
//! task's own locals by move (core-semantics §9.5), because they drop before
//! the group does. The TaskGroup pass (`src/typechecker/core_taskgroup.rs`)
//! now walks a task's body with the groups that are open around it, so the
//! inner spawn is checked against the outer group. The by-move half is pinned
//! by `corpus/core/ok_taskgroup_nested_task_moves_outer_task_local`; these
//! pin that a later use of the moved local in the outer task's body no longer
//! runs on a stale reference.

use std::path::PathBuf;
use std::process::Command;

const USED_AFTER: &str = "import std.sync.{TaskGroup};
fn main() {
    let g = TaskGroup.new();
    let outer = g.spawn(|| {
        let inner_label = \"abc\".to_string();
        let h = g.spawn(|| inner_label.len());
        let n = inner_label.len();
        h.join() + n
    });
    println(outer.join());
}
";

const DECLARED_BEFORE: &str = "import std.sync.{TaskGroup};
fn main() {
    let label = \"abcd\".to_string();
    let g = TaskGroup.new();
    let outer = g.spawn(|| {
        let h = g.spawn(|| label.len());
        h.join() + label.len()
    });
    println(outer.join());
}
";

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-nested-task-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    file
}

fn karac(args: &[&str]) -> (Option<i32>, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_AUTO_PAR", "0")
        .args(args)
        .output()
        .expect("karac");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// PREDICTS B-2026-11-27-66: `karac check` reports E0500 at the second use;
// today the MIR move check stops the run instead.
#[test]
fn an_outer_task_local_used_after_an_inner_spawn_does_not_run() {
    let f = program("used-after", USED_AFTER);
    let (code, out, err) = karac(&["run", "--interp", f.to_str().unwrap()]);
    assert_ne!(code, Some(0), "{out}{err}");
    assert!(err.contains("moved"), "{err}");
}

#[test]
fn a_main_local_declared_before_the_group_is_still_borrowed_by_both_tasks() {
    let f = program("declared-before", DECLARED_BEFORE);
    let (code, _, err) = karac(&["check", f.to_str().unwrap()]);
    assert_eq!(code, Some(0), "{err}");
    let (code, out, err) = karac(&["run", "--interp", f.to_str().unwrap()]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(out, "8\n");
}
