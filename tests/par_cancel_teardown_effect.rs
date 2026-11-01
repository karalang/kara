//! A program that cancels anything gives each `TaskGroup.spawn` a
//! cancellation point, whose teardown drops every live local of the
//! enclosing frame and returns (core-semantics §11.9). Inside a `par`, that
//! teardown sat in each branch's blocks, so the drop of the task's OUTER
//! local, whose `Drop` prints, was charged to every branch and `par { 1, 2 }`
//! was refused as a `writes(Stdout)` conflict (B-2026-11-01-37). The outer
//! frame's teardown runs once, after the `par` is torn down, so it is no
//! branch's effect; a branch's own work, and a drop of a local the branch
//! owns, still conflict.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-par-cancel-teardown-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

const HEAD: &str = "struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f\"d{self.id}\"); } }
fn main() {
    let g = TaskGroup.new();
    let h = g.spawn(|| {
        let r = R { id: 1 };
";

const TAIL: &str = "        a + b
    });
    println(h.join());
    g.cancel();
}
";

#[test]
fn an_outer_local_s_cancellation_teardown_is_no_par_branch_s_effect() {
    let src = format!("{HEAD}        let (a, b) = par {{ 1, 2 }};\n{TAIL}");
    let path = temp("outer", &src);
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    assert!(
        check.status.success(),
        "{}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        (
            run.status.code(),
            String::from_utf8_lossy(&run.stdout).as_ref()
        ),
        (Some(0), "d1\n3\n"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn par_branches_that_print_or_drop_their_own_printing_local_still_conflict() {
    for (tag, par) in [
        (
            "prints",
            "par { { println(\"x\"); 1 }, { println(\"y\"); 2 } }",
        ),
        (
            "own-drop",
            "par { { let q = R { id: 2 }; 1 }, { let q = R { id: 3 }; 2 } }",
        ),
    ] {
        let src = format!("{HEAD}        let (a, b) = {par};\n{TAIL}");
        let path = temp(tag, &src);
        let check = karac(&["check", path.to_str().unwrap()]);
        let _ = std::fs::remove_file(&path);
        let err = String::from_utf8_lossy(&check.stderr);
        assert!(
            !check.status.success()
                && err.contains("branches 1 and 2 of the `par` conflict: `writes(Stdout)`"),
            "{tag}: {err}"
        );
    }
}
