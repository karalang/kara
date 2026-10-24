//! A bare `for` over a collection borrows it (core semantics §4.6), so a
//! binding of its element, or of a pattern over the element, is a view:
//! `karac check` binds it as a `ref`, and `*o` reads through it. The MIR
//! pipeline binds the same names at their value types and reads `*o` as the
//! binding, so a program check accepts runs, and prints the same, on the
//! interpreter and the compiled backend (B-2026-10-23-8).
#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::Command;

/// The program's stdout from `karac <cmd> main.kara`, after `karac check`
/// accepted it.
fn checked_and_run(name: &str, src: &str, cmd: &str) -> Option<String> {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("karac-for-views-{name}-{}", std::process::id()));
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

#[test]
fn mir_deref_of_a_destructured_for_element() {
    let src = r#"fn names() -> Vec[(String, i64)] {
    let mut v: Vec[(String, i64)] = Vec.new();
    v.push(("a".to_string(), 1));
    v.push(("b".to_string(), 2));
    v
}
fn main() {
    let want = "b".to_string();
    let ns = names();
    for (o, n) in ns {
        if *o == want { println(f"{n} {*n}"); }
    }
    for (o, _) in names() {
        if *o != want { println(f"{o}"); }
    }
}
"#;
    runs_everywhere("tuple", src, "2 2\na\n");
}

#[test]
fn mir_deref_of_an_if_let_binding_over_a_for_element() {
    let src = r#"fn main() {
    let mut all: Vec[Option[String]] = Vec.new();
    all.push(Some("a".to_string()));
    all.push(None);
    let t = "a".to_string();
    for g in all {
        if let Some(f) = g {
            if t == *f { println("eq"); }
        }
    }
    println(f"{all.len()}");
}
"#;
    runs_everywhere("if-let", src, "eq\n2\n");
}

#[test]
fn mir_deref_of_a_match_binding_over_a_borrowed_field_element() {
    let src = r#"enum Seg {
    Text(String),
    Code(i64),
}
struct Payload {
    segs: Vec[Seg],
}
fn total(p: ref Payload) -> i64 {
    let mut n: i64 = 0;
    for s in p.segs {
        match s {
            Seg.Code(v) => n += *v,
            Seg.Text(t) => n += t.len() as i64,
        }
    }
    n
}
fn main() {
    let p = Payload { segs: vec![Seg.Code(5), Seg.Text("abc".to_string())] };
    println(f"{total(p)}");
}
"#;
    runs_everywhere("match", src, "8\n");
}
