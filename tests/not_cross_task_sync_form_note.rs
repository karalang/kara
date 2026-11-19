//! B-2026-11-19-17: design.md § From `shared` to `sync` promised a
//! suggested diff with E_NOT_CROSS_TASK. The refusal now carries the
//! `sync` form of the program's `shared struct` as a note to review: each
//! `mut` field loses `mut` and goes in an `Atomic[T]` (`bool`, `i32`,
//! `i64`, `u32`, `u64`) or a `Mutex[T]`. It is deliberately not a machine
//! fix (the design says no fix in this area is safe without review), so
//! `karac fix` leaves the file alone; the suggested declaration checks
//! and runs once the field writes are adapted.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

const SRC: &str = "enum Direction { Up, Down }
shared struct Elevator { stops: Vec[i64], mut direction: Direction, mut floor: i64 }
fn run(e: Elevator) -> i64 { e.floor }
fn main() {
    let state = Elevator { stops: Vec.new(), direction: Direction.Up, floor: 0 };
    let (a, b) = par { run(state), run(state) };
    println(a + b);
}
";

#[test]
fn not_cross_task_shows_the_sync_form_for_review_only() {
    let path = std::env::temp_dir().join(format!("karac-b1917-{}.kara", std::process::id()));
    std::fs::write(&path, SRC).unwrap();
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    let err = String::from_utf8_lossy(&check.stderr);
    assert!(err.contains("error[E_NOT_CROSS_TASK]"), "{err}");
    assert!(
        err.contains(
            "  - shared struct Elevator { stops: Vec[i64], mut direction: Direction, mut floor: i64 }\n  \
             + sync struct Elevator { stops: Vec[i64], direction: Mutex[Direction], floor: Atomic[i64] }"
        ),
        "{err}"
    );
    karac(&["fix", file]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), SRC);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_suggested_sync_form_checks_and_runs() {
    let src = "enum Direction { Up, Down }
sync struct Elevator { stops: Vec[i64], direction: Mutex[Direction], floor: Atomic[i64] }
fn run(e: Elevator) -> i64 { e.floor.load() }
fn main() {
    let state = Elevator { stops: Vec.new(), direction: Mutex.new(Direction.Up), floor: Atomic.new(2) };
    let (a, b) = par { run(state), run(state) };
    println(a + b);
}
";
    let path = std::env::temp_dir().join(format!("karac-b1917-sync-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "4\n");
}
