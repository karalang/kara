//! A plain `dyn Trait` is not `CrossTask` (design.md § `dyn Trait`), so a
//! task may view one but never take it by move: the concrete value may hold
//! a `shared` handle, whose count is not atomic. `karac check` skipped every
//! `dyn` capture of a task, because the capture check sees no modes
//! (B-2026-10-30-13); the ownership pass, which has them, now refuses a
//! move. `dyn Trait + CrossTask` crosses.

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-dyntask-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const JOB: &str = "trait Job { fn run(ref self) -> i64; }
shared struct Cell { mut n: i64 }
struct J { c: Cell }
impl Job for J { fn run(ref self) -> i64 { self.c.n } }
struct K { n: i64 }
impl Job for K { fn run(ref self) -> i64 { self.n } }
";

#[test]
fn a_task_cannot_take_a_plain_dyn_by_move() {
    for (name, body) in [
        (
            "moved_by_the_body",
            "    let j: dyn Job = J { c: Cell { n: 3 } };
    let g = TaskGroup.new();
    let h = g.spawn(|| { let k = j; k.run() });
    println(h.join());",
        ),
        (
            "dropped_before_the_group",
            "    let g = TaskGroup.new();
    let j: dyn Job = J { c: Cell { n: 3 } };
    let h = g.spawn(|| j.run());
    println(h.join());",
        ),
    ] {
        let src = format!("{JOB}fn main() {{\n{body}\n}}\n");
        let (ok, err) = check(name, &src);
        assert!(!ok, "{name}: check let a task take a plain `dyn` by move");
        assert!(
            err.contains(
                "`j` is a `dyn Job`, which is not `CrossTask`, and this task takes it by move"
            ),
            "{name}: {err}"
        );
    }
}

#[test]
fn a_task_may_view_a_plain_dyn_or_move_a_cross_task_one() {
    for (name, body, want) in [
        (
            "view",
            "    let j: dyn Job = K { n: 3 };
    let g = TaskGroup.new();
    let h = g.spawn(|| j.run());
    println(h.join());",
            "3\n",
        ),
        (
            "cross_task_move",
            "    let j: dyn Job + CrossTask = K { n: 4 };
    let g = TaskGroup.new();
    let h = g.spawn(|| { let k = j; k.run() });
    println(h.join());",
            "4\n",
        ),
    ] {
        let src = format!("{JOB}fn main() {{\n{body}\n}}\n");
        let (ok, err) = check(name, &src);
        assert!(ok, "{name}: {err}");
        assert_eq!(
            karac::mir::lower::run_source(&src)
                .expect("MIR interpreter")
                .output,
            want,
            "{name}"
        );
    }
}
