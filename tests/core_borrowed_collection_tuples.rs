//! A returned borrowed collection whose elements are tuples (B-2026-10-27-1).
//! Only a tuple's `ref` members can borrow storage that dies at return, so
//! `(c, 1)` pushed into a returned `Vec[(ref Node, i64)]` is pinned exactly
//! when `c` is, as a bare `c` in a `Vec[ref Node]` is. Check used to read
//! the tuple literal itself as a local and refused the return, where the
//! MIR borrow checker accepts it.

use std::process::Command;

const NODE: &str = "struct Node { val: i64, children: Vec[Node] }\n";
const MAIN: &str = "fn main() {\n    let t = Node { val: 1, children: vec![Node { val: 2, children: Vec.new() }] };\n    println(f\"{frames(t).len()}\");\n}\n";

/// `karac check` on `NODE + frames + MAIN`: its error lines, empty when it
/// passes.
fn check(tag: &str, frames: &str) -> String {
    let src = format!("{NODE}{frames}\n{MAIN}");
    let dir = std::env::temp_dir().join(format!(
        "karac-borrowed-tuples-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, &src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", file.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    let errors: Vec<&str> = err.lines().filter(|l| l.starts_with("error")).collect();
    assert_eq!(out.status.success(), errors.is_empty(), "{err}");
    if errors.is_empty() {
        let r = karac::mir::lower::run_source(&src).unwrap_or_else(|e| panic!("build: {e}"));
        assert_eq!(r.output, "1\n", "{tag}: {}", r.stderr);
    }
    errors.join("\n")
}

const DANGLES: &str =
    "returned borrowed collection holds an element that borrows function-local storage";

#[test]
fn core_borrowed_tuple_of_a_param_borrow_is_returned() {
    for (tag, frames) in [
        (
            "element",
            "fn frames(t: Node) -> Vec[(ref Node, i64)] {\n    let mut v: Vec[(ref Node, i64)] = Vec.new();\n    for c in t.children.iter() {\n        v.push((c, 1));\n    }\n    v\n}\n",
        ),
        (
            "param",
            "fn frames(t: Node) -> Vec[(ref Node, i64)] {\n    let mut v: Vec[(ref Node, i64)] = Vec.new();\n    v.push((t, 1));\n    v\n}\n",
        ),
        (
            "nested",
            "fn frames(t: Node) -> Vec[(i64, (ref Node, String))] {\n    let mut v: Vec[(i64, (ref Node, String))] = Vec.new();\n    v.push((1, (t, \"a\")));\n    v\n}\n",
        ),
    ] {
        assert_eq!(check(tag, frames), "", "{tag}");
    }
}

#[test]
fn core_borrowed_tuple_of_a_local_borrow_is_refused() {
    let err = check(
        "local",
        "fn frames(t: Node) -> Vec[(ref Node, i64)] {\n    let mut v: Vec[(ref Node, i64)] = Vec.new();\n    let local = Node { val: 3, children: Vec.new() };\n    v.push((local, 1));\n    v\n}\n",
    );
    assert!(err.contains(DANGLES), "{err}");
}
