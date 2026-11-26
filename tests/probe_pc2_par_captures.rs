//! Captures through `par` and `TaskGroup.spawn`, per place (core semantics
//! §9.1, §11.2), and a par conflict reported as one whichever branch comes
//! first.
//!
//! - B-2026-11-15-126: a branch or task that reads `p.a` and writes `p.b` was charged
//!   a mutable borrow of `p.a` too: the MIR closure capture gave every field
//!   it named the binding's mode, and the TaskGroup check compared the
//!   binding's `mut ref` against every path the other task named. Now only
//!   the fields a body changes are captured mutably, and a task's `mut ref`
//!   is exclusive over the paths it writes.
//! - B-2026-11-15-127: a move inside a `par` branch inside a closure was missed by
//!   capture inference, so the closure captured the moved value by `ref`
//!   and the move was refused.
//! - B-2026-11-15-128: `par { { eat(s1) }, { s1.len() } }` was reported as a
//!   sequential use after move (E0500); the branches run at once, so both
//!   orders give E0418 naming both accesses. A move in a branch still
//!   holds after the `par`.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(name: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path
}

/// `karac check`'s verdict and stderr.
fn check(name: &str, src: &str) -> (bool, String) {
    let path = write(name, src);
    let out = karac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// The program's stdout under the MIR interpreter, asserting it ran.
fn run(name: &str, src: &str) -> String {
    let path = write(name, src);
    let out = karac(&["__mir-run", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

const PAR_READ_A_WRITE_B: &str = r#"struct P { a: i64, b: i64 }
fn main() {
    let mut p = P { a: 1, b: 2 };
    let (r1, r2) = par {
        { let t = p.a; t },
        { let t = p.a; p.b = 2; t },
    };
    println(f"{r1} {r2} {p.a} {p.b}");
}
"#;
const PAR_WRITE_A_BOTH: &str = r#"struct P { a: i64, b: i64 }
fn main() {
    let mut p = P { a: 1, b: 2 };
    let (r1, r2) = par {
        { let t = p.a; t },
        { let t = p.b; p.a = 2; t },
    };
    println(f"{r1} {r2} {p.a} {p.b}");
}
"#;
const TASK_READ_A_WRITE_B: &str = r#"import std.sync.{TaskGroup};
struct P { a: i64, b: i64 }
fn main() {
    let mut p = P { a: 1, b: 2 };
    {
        let g = TaskGroup.new();
        let h1 = g.spawn(|| p.a);
        let h2 = g.spawn(|| { let t = p.a; p.b = 5; t });
        println(f"{h1.join()} {h2.join()}");
    }
    println(f"{p.a} {p.b}");
}
"#;
const TASK_WRITE_A_BOTH: &str = r#"import std.sync.{TaskGroup};
struct P { a: i64, b: i64 }
fn main() {
    let mut p = P { a: 1, b: 2 };
    {
        let g = TaskGroup.new();
        let h1 = g.spawn(|| p.a);
        let h2 = g.spawn(|| { let t = p.b; p.a = 5; t });
        println(f"{h1.join()} {h2.join()}");
    }
    println(f"{p.a} {p.b}");
}
"#;
const MOVE_IN_PAR_IN_CLOSURE: &str = r#"fn eat(s: own String) -> i64 { s.len() }
fn run(f: own OnceFn() -> i64) -> i64 { f() }
fn main() {
    let s = "abc".to_string();
    println(run(|| { let (x, y) = par { eat(s), 1 }; x + y }));
}
"#;
const MOVE_THEN_READ: &str = r#"fn eat(s: own String) -> i64 { s.len() }
fn main() {
    let s1 = "abc".to_string();
    let (a, b) = par {
        { eat(s1) },
        { s1.len() },
    };
    println(f"{a} {b}");
}
"#;
const READ_THEN_MOVE: &str = r#"fn eat(s: own String) -> i64 { s.len() }
fn main() {
    let s1 = "abc".to_string();
    let (a, b) = par {
        { s1.len() },
        { eat(s1) },
    };
    println(f"{a} {b}");
}
"#;

#[test]
fn probe_pc2_par_branch_reading_one_field_and_writing_another_runs() {
    let (ok, stderr) = check("pc2-par-ab", PAR_READ_A_WRITE_B);
    assert!(ok, "{stderr}");
    assert_eq!(run("pc2-par-ab", PAR_READ_A_WRITE_B), "1 1 1 2\n");
}

#[test]
fn probe_pc2_par_branch_writing_the_field_another_reads_still_conflicts() {
    let (ok, stderr) = check("pc2-par-aa", PAR_WRITE_A_BOTH);
    assert!(!ok, "a write of p.a beside a read of p.a was accepted");
    assert!(stderr.contains("p.0") || stderr.contains("p.a"), "{stderr}");
}

#[test]
fn probe_pc2_task_reading_one_field_and_writing_another_runs() {
    let (ok, stderr) = check("pc2-task-ab", TASK_READ_A_WRITE_B);
    assert!(ok, "{stderr}");
    assert_eq!(run("pc2-task-ab", TASK_READ_A_WRITE_B), "1 1\n1 5\n");
}

#[test]
fn probe_pc2_task_writing_the_field_another_reads_still_conflicts() {
    let (ok, stderr) = check("pc2-task-aa", TASK_WRITE_A_BOTH);
    assert!(
        !ok,
        "a task writing p.a beside one reading p.a was accepted"
    );
    assert!(stderr.contains("by `mut ref`"), "{stderr}");
}

#[test]
fn probe_tk1_a_move_in_a_par_branch_inside_a_closure_moves_the_capture() {
    let (ok, stderr) = check("tk1", MOVE_IN_PAR_IN_CLOSURE);
    assert!(ok, "{stderr}");
    assert_eq!(run("tk1", MOVE_IN_PAR_IN_CLOSURE), "4\n");
}

/// `karac check --output=json`'s stdout.
fn check_json(name: &str, src: &str) -> String {
    let path = write(name, src);
    let out = karac(&["check", "--output=json", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn probe_pc3_a_move_and_a_read_in_two_branches_conflict_in_either_order() {
    for (name, src, text) in [
        ("pc3-mv1", MOVE_THEN_READ, "`move of s1` and `read of s1`"),
        ("pc3-mv2", READ_THEN_MOVE, "`read of s1` and `move of s1`"),
    ] {
        let (ok, stderr) = check(name, src);
        assert!(!ok, "{name} was accepted");
        assert!(stderr.contains(text), "{name}: {stderr}");
        assert!(!stderr.contains("used again here"), "{name}: {stderr}");
        let json = check_json(name, src);
        assert!(json.contains("\"code\":\"E0418\""), "{name}: {json}");
    }
}

#[test]
fn probe_pc3_a_move_in_a_branch_then_a_use_after_the_par_is_still_a_use_after_move() {
    let src = r#"fn eat(s: own String) -> i64 { s.len() }
fn main() {
    let s1 = "abc".to_string();
    let (a, b) = par {
        { eat(s1) },
        { 2 },
    };
    println(f"{a} {b} {s1.len()}");
}
"#;
    let (ok, stderr) = check("pc3-after", src);
    assert!(
        !ok,
        "a use after the par of a value a branch moved was accepted"
    );
    assert!(stderr.contains("used again here"), "{stderr}");
}

#[test]
fn probe_tk1_a_move_in_a_par_branch_inside_a_spawned_task_moves_the_capture() {
    let src = r#"import std.sync.{TaskGroup};

fn eat(s: own String) -> i64 { s.len() }
fn main() {
    let s = "abc".to_string();
    let g = TaskGroup.new();
    let h = g.spawn(|| { let (x, y) = par { eat(s), 1 }; x + y });
    println(h.join());
}
"#;
    let (ok, stderr) = check("tk1-task", src);
    assert!(ok, "{stderr}");
    assert_eq!(run("tk1-task", src), "4\n");
}
