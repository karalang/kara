//! The compiled-code fixes from the second x86 Callgrind sweep of the
//! katas (kata 1516, 298), pinned by their answers on the interpreter and
//! the built binary and by the shape of the IR:
//! - a read-only walk over a `mut` children vector in a count-safe body
//!   checks the field's borrow flag instead of taking and giving it back,
//!   and a conflicting writer through another handle still panics
//!   (B-2026-11-16-241);
//! - a handle bound out of a borrowed node's `mut` field (`match n.left {
//!   Some(c) => .. }`) in a count-safe body counts nothing, a `mut ref i64`
//!   parameter included (B-2026-11-16-242).
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const PROG: &str = r#"shared struct Node {
    val: i64,
    mut children: Vec[Node],
}

shared struct Tree {
    val: i64,
    mut left: Option[Tree],
    mut right: Option[Tree],
}

fn has(node: Node, val: i64) -> bool {
    if node.val == val {
        return true;
    }
    for child in node.children.iter() {
        if has(child, val) {
            return true;
        }
    }
    false
}

fn longest(t: Option[Tree], best: mut ref i64) -> i64 {
    match t {
        None => 0,
        Some(n) => {
            let l = longest(n.left, mut best);
            let r = longest(n.right, mut best);
            let mut run = 1;
            match n.left {
                None => {}
                Some(c) => {
                    if c.val == n.val + 1 and l + 1 > run { run = l + 1; }
                }
            }
            match n.right {
                None => {}
                Some(c) => {
                    if c.val == n.val + 1 and r + 1 > run { run = r + 1; }
                }
            }
            if run > *best { *best = run; }
            run
        }
    }
}

fn sum_kids(n: Node) -> i64 {
    let mut s = 0;
    for c in n.children.iter() {
        s += c.val;
    }
    s
}

fn main() {
    let root = Node { val: 0, children: Vec.new() };
    let a = Node { val: 1, children: Vec.new() };
    a.children.push(Node { val: 2, children: Vec.new() });
    root.children.push(a);
    root.children.push(Node { val: 3, children: Vec.new() });
    println(f"{has(root, 2)} {has(root, 4)} {sum_kids(root)}");
    let t = Some(Tree { val: 1, left: Some(Tree { val: 2, left: Some(Tree { val: 3, left: None, right: None }), right: None }), right: Some(Tree { val: 5, left: None, right: None }) });
    let mut best = 0;
    let _ = longest(t, mut best);
    println(f"{best}");
    let alias = root.clone();
    root.children.retain(|c| sum_kids(alias) > c.val);
    println("unreached");
}
"#;

const OUT: &str = "true false 4\n3\n";
const PANIC: &str = "runtime error: already borrowed: `Node.children` is borrowed through another handle at main.kara:69:5 (core semantics §6.2)\n  at main.kara:51:5\n";

fn karac() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_AUTO_PAR", "0");
    c
}

/// The lines of the function `name` in `ir`.
fn function<'a>(ir: &'a str, name: &str) -> String {
    let head = format!("@kara.{name}(");
    let f: Vec<&'a str> = ir
        .lines()
        .skip_while(|l| !(l.starts_with("define") && l.contains(&head)))
        .take_while(|l| *l != "}")
        .collect();
    assert!(!f.is_empty(), "no {name} in the IR");
    f.join("\n")
}

#[test]
fn mir_kata_sweep2_reads_take_no_flag_and_no_count() {
    let d = std::env::temp_dir().join(format!("karac-kata-sweep2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let file = d.join("main.kara");
    std::fs::write(&file, PROG).unwrap();
    let interp = karac()
        .current_dir(&d)
        .args(["__mir-run", "main.kara"])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&interp.stderr);
    assert_eq!(String::from_utf8_lossy(&interp.stdout), OUT, "{err}");
    assert!(err.starts_with(PANIC), "{err}");
    assert_eq!(interp.status.code(), Some(101));
    let exe = d.join("main");
    let ll = d.join("main.ll");
    let built = karac()
        .current_dir(&d)
        .env("KARAC_MIR_LL", &ll)
        .args(["build", "main.kara", "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    if common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why)
    })
    .is_none()
    {
        return;
    }
    let ran = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&ran.stdout), OUT);
    assert!(String::from_utf8_lossy(&ran.stderr).starts_with(PANIC));
    assert_eq!(ran.status.code(), Some(101));
    let ir = std::fs::read_to_string(&ll).unwrap();
    for name in ["has", "sum_kids"] {
        let f = function(&ir, name);
        assert!(
            !f.contains("karac_mir_flag_acquire"),
            "{name} takes a flag:\n{f}"
        );
        assert!(
            f.contains("karac_mir_flag_check"),
            "{name} checks no flag:\n{f}"
        );
    }
    let f = function(&ir, "longest");
    assert!(!f.contains("%count"), "longest counts a handle:\n{f}");
    let _ = std::fs::remove_dir_all(&d);
}
