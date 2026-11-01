//! What the MIR builder refuses, `karac check` refuses, at the position the
//! build names; and a function the builder refuses leaves the MIR borrow
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

/// `grow`'s E0516 is the MIR borrow check's, which check used to skip for
/// the whole program while any other function failed to lower; `take`'s
/// `??` was one (B-2026-11-01-1) and lowers now, so check reports exactly
/// the one error.
#[test]
fn check_reports_a_mir_borrow_error_beside_a_function_that_lowers() {
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
    assert!(!all.contains("cannot move"), "{all}");
    assert!(
        all.contains(":13:5: cannot mutably borrow `v` while `first`"),
        "{all}"
    );
    assert!(all.contains("1 error(s) found"), "{all}");
}

/// The struct variant named like a library type that B-2026-11-01-6 made
/// the builder refuse (check reported it at 8:33 once B-2026-11-01-2
/// landed, the builder-refusal example here) checks and runs since that
/// row's fix; a builder refusal's position is pinned by
/// `lowering_messages_split_at_each_position`.
#[test]
fn a_struct_variant_named_like_a_library_type_checks_and_runs() {
    let path = program(
        "builder",
        "enum Stmt {\n    Match { cases: Vec[i64] },\n    Other,\n}\n\n\
         fn visit(s: ref Stmt) -> i64 {\n    match s {\n        \
         Stmt.Match { cases } => cases.len(),\n        Stmt.Other => 0,\n    }\n}\n\n\
         fn main() {\n    println(visit(Stmt.Match { cases: vec![1, 2] }));\n}\n",
    );
    let out = check(&path);
    assert!(out.status.success(), "{}", text(&out));
    let run = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-run")
        .arg(&path)
        .current_dir(path.parent().unwrap())
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "2\n",
        "{}",
        text(&run)
    );
}
