//! Found by kata 1161's Rust arm (B-2026-11-15-17).
//!
//! LeetCode's binary-tree template opens with `use std::cell::RefCell;`,
//! `use std::collections::VecDeque;` and `use std::rc::Rc;`. Each line has
//! a diagnostic whose fix deletes it, but after the first one the parser's
//! item recovery skipped to the next `fn`, past the other `use` lines, so
//! `karac check` reported one and `karac fix` deleted one per round.

use std::process::{Command, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("karac-k1161-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path
}

const THREE: &str = "use std::cell::RefCell;\nuse std::collections::VecDeque;\nuse std::rc::Rc;\n\nfn main() {\n    println(\"hi\");\n}\n";

#[test]
fn each_rust_use_line_reports_its_own_diagnostic() {
    let path = write("check", THREE);
    let check = karac(&["check"], &path);
    let _ = std::fs::remove_file(&path);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    for written in [
        "std::cell::RefCell",
        "std::collections::VecDeque",
        "std::rc::Rc",
    ] {
        assert!(
            err.contains(&format!("`{written}` is Rust's standard library")),
            "{written}: {err}"
        );
    }
}

#[test]
fn three_rust_use_lines_go_in_one_fix_round() {
    let path = write("fix", THREE);
    let fix = karac(&["fix"], &path);
    let fixed = std::fs::read_to_string(&path).unwrap();
    let check = karac(&["check"], &path);
    let _ = std::fs::remove_file(&path);
    assert!(
        String::from_utf8_lossy(&fix.stdout).contains("applied 3 fix(es)"),
        "{}",
        String::from_utf8_lossy(&fix.stdout)
    );
    assert_eq!(fixed, "fn main() {\n    println(\"hi\");\n}\n");
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
}

#[test]
fn a_use_line_between_items_still_reports_each() {
    let src = "use std::collections::HashMap;\n\nfn f() -> i64 {\n    1\n}\n\nuse std::collections::HashSet;\nuse std::cmp::max;\n\nfn main() {\n    println(f\"{f()}\");\n}\n";
    let path = write("between", src);
    let check = karac(&["check"], &path);
    let _ = std::fs::remove_file(&path);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    for written in [
        "std::collections::HashMap",
        "std::collections::HashSet",
        "std::cmp::max",
    ] {
        assert!(
            err.contains(&format!("`{written}` is Rust's standard library")),
            "{written}: {err}"
        );
    }
}
