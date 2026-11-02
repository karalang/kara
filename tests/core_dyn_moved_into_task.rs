//! A plain `dyn Trait` is not `CrossTask` (design.md § `dyn Trait`): the
//! concrete value may hold a `shared` handle, whose count is not atomic.
//! A task once could view one and only a move was refused (B-2026-10-30-13);
//! since B-2026-11-02-12 a task may not reach one at all, by move or by
//! view, and `karac fix` writes `+ CrossTask` where its type is declared.
//! `dyn Trait + CrossTask` crosses either way.

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
fn a_task_cannot_reach_a_plain_dyn() {
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
        (
            "viewed",
            "    let j: dyn Job = K { n: 3 };
    let g = TaskGroup.new();
    let h = g.spawn(|| j.run());
    println(h.join());",
        ),
        (
            "viewed_by_a_branch",
            "    let j: dyn Job = K { n: 3 };
    let (a, b) = par { j.run(), j.run() };
    println(a + b);",
        ),
    ] {
        let src = format!("{JOB}fn main() {{\n{body}\n}}\n");
        let (ok, err) = check(name, &src);
        assert!(!ok, "{name}: check let a task reach a plain `dyn`");
        assert!(
            err.contains("capture of `j` (type `dyn Job`) cannot cross")
                && err.contains("write `dyn Job + CrossTask`"),
            "{name}: {err}"
        );
    }
}

#[test]
fn a_task_may_view_or_move_a_cross_task_dyn() {
    for (name, body, want) in [
        (
            "cross_task_view",
            "    let j: dyn Job + CrossTask = K { n: 3 };
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

/// The fix-it marks the `dyn` where it is declared, before any `with`, and
/// the marked type then checks the value coerced into it.
#[test]
fn the_fix_marks_a_plain_dyn_where_it_is_declared() {
    let dir = std::env::temp_dir().join(format!("karac-dyntask-fix-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    let src = format!(
        "{JOB}fn mk(n: i64) -> dyn Job with reads(Clock) {{ K {{ n: n }} }}
fn main() {{
    let j: dyn Job = mk(5);
    let g = TaskGroup.new();
    let h = g.spawn(|| j.run());
    println(h.join());
}}
"
    );
    std::fs::write(&path, &src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("fix")
        .arg(&path)
        .output()
        .unwrap();
    let fixed = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        fixed.contains("let j: dyn Job + CrossTask = mk(5);"),
        "fix wrote:\n{fixed}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // `mk`'s plain `dyn Job` does not coerce into the marked one, and the
    // check says so rather than letting the value through.
    let (ok, err) = check("after_fix", &fixed);
    assert!(
        !ok,
        "a plain `dyn` from `mk` passed as `+ CrossTask`:\n{fixed}"
    );
    assert!(err.contains("CrossTask"), "{err}");
}
