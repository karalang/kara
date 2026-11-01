//! What the MIR builder refuses, `karac check` refuses, at the position the
//! build names; and a forbidden move in one function leaves the MIR borrow
//! check of every other function in place (B-2026-11-01-2).

use std::path::PathBuf;
use std::process::{Command, Output};

fn program(tag: &str, src: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("karac-check-lowering-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn check(path: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(path)
        .current_dir(path.parent().unwrap())
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The `??` in `take` is a move the build refuses (PREDICTS
/// B-2026-11-01-1: once a borrowed handle aggregate counts, only the
/// E0516 remains), and `grow`'s E0516 is the MIR borrow check's, which
/// check used to skip for the whole program.
#[test]
fn check_reports_a_forbidden_move_and_another_function_s_borrow_error() {
    let path = program(
        "moves",
        "shared enum Node {\n    Leaf(i64),\n    Pair(Node, Node),\n}\n\n\
         fn take(d: Option[Node]) -> Node {\n    d ?? Node.Leaf(0)\n}\n\n\
         fn grow() {\n    let mut v: Vec[String] = vec![\"a\".to_string()];\n    \
         let first = v.first();\n    v.push(\"b\".to_string());\n    println(first ?? \"none\");\n}\n\n\
         fn main() {\n    grow();\n    let _t = take(Some(Node.Leaf(2)));\n}\n",
    );
    let out = check(&path);
    let all = text(&out);
    assert!(!out.status.success(), "{all}");
    assert!(!all.contains("All checks passed"), "{all}");
    assert!(
        all.contains(":7:5: cannot move a non-`Copy` value out of a borrowed place"),
        "{all}"
    );
    assert!(
        all.contains(":13:5: cannot mutably borrow `v` while `first`"),
        "{all}"
    );
}

/// A builder error (here a struct variant named like a library type,
/// PREDICTS B-2026-11-01-6: the program prints 2 once it is fixed) is
/// check's error at its position, without the builder's function name.
#[test]
fn check_reports_a_builder_refusal_at_its_position() {
    let path = program(
        "builder",
        "enum Stmt {\n    Match { cases: Vec[i64] },\n    Other,\n}\n\n\
         fn visit(s: ref Stmt) -> i64 {\n    match s {\n        \
         Stmt.Match { cases } => cases.len(),\n        Stmt.Other => 0,\n    }\n}\n\n\
         fn main() {\n    println(visit(Stmt.Match { cases: vec![1, 2] }));\n}\n",
    );
    let out = check(&path);
    let all = text(&out);
    assert!(!out.status.success(), "{all}");
    assert!(
        all.contains(":8:33: a type that failed to check is not lowered yet"),
        "{all}"
    );
}
