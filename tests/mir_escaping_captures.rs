//! A closure passed straight to an `escaping` parameter outlives the frame
//! (core semantics §9.3), so what it captures cannot be a borrow of that
//! frame: a `shared` handle is a counted copy and the caller's handle stays
//! usable (§6.1), and a borrowed parameter that cannot move out gives the
//! closure the `Copy` and handle fields it uses (§9.1). These printed a
//! wrong answer, crashed or stopped on a false move error, after `karac
//! check` accepted them (B-2026-10-25-16, B-2026-10-25-17).
#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::Command;

/// The program's stdout from `karac <cmd> main.kara`, after `karac check`
/// accepted it.
fn checked_and_run(name: &str, src: &str, cmd: &str) -> Option<String> {
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "karac-escaping-captures-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let check = Command::new(karac)
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "{name}: check: {}",
        String::from_utf8_lossy(&check.stdout)
    );
    let out = Command::new(karac).arg(cmd).arg(&file).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if cmd == "__mir-llvm-run" {
        common::link_or_skip(if out.status.success() {
            Ok(())
        } else {
            Err(err.clone())
        })?;
    }
    assert!(out.status.success(), "{name}: {cmd}: {err}");
    Some(String::from_utf8(out.stdout).unwrap())
}

fn runs_everywhere(name: &str, src: &str, want: &str) {
    for cmd in ["__mir-run", "__mir-llvm-run"] {
        if let Some(out) = checked_and_run(name, src, cmd) {
            assert_eq!(out, want, "{name}: {cmd}");
        }
    }
}

/// A bare `shared` parameter, read through a field, kept in a struct field
/// (B-2026-10-25-17's program).
#[test]
fn mir_escaping_closure_counts_a_borrowed_handle_parameter() {
    let src = r#"shared struct P {
    n: i64,
}

struct Op {
    run: Fn(i64) -> i64,
}

fn new_op(run: escaping Fn(i64) -> i64) -> Op {
    Op { run: run }
}

fn adder(p: P) -> Op {
    new_op(|x| x + p.n)
}

fn main() {
    let op = adder(P { n: 5 });
    let junk = Vec[P { n: 100 }, P { n: 200 }];
    let a = (op.run)(1);
    let b = (op.run)(2);
    println(f"{a} {b} {junk.len()}");
}
"#;
    runs_everywhere("handle-param", src, "6 7 2\n");
}

/// The same with a `shared` holder.
#[test]
fn mir_escaping_closure_counts_a_handle_into_a_shared_holder() {
    let src = r#"shared struct P {
    n: i64,
}

shared struct Op {
    run: Fn(i64) -> i64,
}

fn new_op(run: escaping Fn(i64) -> i64) -> Op {
    Op { run: run }
}

fn adder(p: P) -> Op {
    new_op(|x| x + p.n)
}

fn main() {
    let op = adder(P { n: 5 });
    let junk = Vec[P { n: 100 }, P { n: 200 }];
    let a = (op.run)(1);
    let b = (op.run)(2);
    println(f"{a} {b} {junk.len()}");
}
"#;
    runs_everywhere("shared-holder", src, "6 7 2\n");
}

/// The same with the whole handle passed on to a function.
#[test]
fn mir_escaping_closure_counts_a_handle_it_passes_whole() {
    let src = r#"shared struct P {
    n: i64,
}

struct Op {
    run: Fn(i64) -> i64,
}

fn new_op(run: escaping Fn(i64) -> i64) -> Op {
    Op { run: run }
}

fn size(p: P) -> i64 { p.n }

fn adder(p: P) -> Op {
    new_op(|x| x + size(p))
}

fn main() {
    let op = adder(P { n: 5 });
    let junk = Vec[P { n: 100 }, P { n: 200 }];
    let a = (op.run)(1);
    let b = (op.run)(2);
    println(f"{a} {b} {junk.len()}");
}
"#;
    runs_everywhere("whole-handle", src, "6 7 2\n");
}

/// Two escaping closures over one local handle, which stays usable after
/// both (B-2026-10-25-16's program).
#[test]
fn mir_two_escaping_closures_share_a_local_handle() {
    let src = r#"shared struct P {
    n: i64,
}

fn keep(f: escaping Fn(i64) -> i64) -> Vec[Fn(i64) -> i64] {
    Vec[f]
}

fn main() {
    let p = P { n: 5 };
    let fs = keep(|x| x + p.n);
    let gs = keep(|x| x * p.n);
    println(f"{p.n}");
    let a = (fs[0])(1);
    let b = (gs[0])(2);
    println(f"{a} {b}");
}
"#;
    runs_everywhere("local-handle", src, "5\n6 10\n");
}

/// Two escaping closures over a borrowed handle parameter, and an escaping
/// `let` closure over a local one.
#[test]
fn mir_escaping_closures_over_parameter_and_let_handles() {
    let src = r#"shared struct P {
    n: i64,
}

fn keep(f: escaping Fn(i64) -> i64) -> Vec[Fn(i64) -> i64] {
    Vec[f]
}

fn twice(p: P) -> Vec[Fn(i64) -> i64] {
    let mut v = keep(|x| x + p.n);
    v.extend(keep(|x| x * p.n));
    v
}

fn main() {
    let p = P { n: 3 };
    let fs = twice(p);
    let q = P { n: 4 };
    let f = |x: i64| x - q.n;
    let gs = keep(f);
    println(f"{(fs[0])(1)} {(fs[1])(2)} {(gs[0])(10)} {p.n} {q.n}");
}
"#;
    runs_everywhere("param-and-let", src, "4 6 6 3 4\n");
}

/// A borrowed struct that cannot move out (a `String` field): the closure
/// copies the `i64` field it reads.
#[test]
fn mir_escaping_closure_copies_the_field_of_a_borrowed_struct() {
    let src = r#"struct P {
    s: String,
    n: i64,
}

struct Op {
    run: Fn(i64) -> i64,
}

fn new_op(run: escaping Fn(i64) -> i64) -> Op {
    Op { run: run }
}

fn adder(p: P) -> Op {
    new_op(|x| x + p.n)
}

fn main() {
    let op = adder(P { s: "a", n: 5 });
    let junk = Vec[P { s: "b", n: 100 }, P { s: "c", n: 200 }];
    let a = (op.run)(1);
    let b = (op.run)(2);
    println(f"{a} {b} {junk.len()}");
}
"#;
    runs_everywhere("borrowed-struct", src, "6 7 2\n");
}
