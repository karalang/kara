//! `karac fix` carries two edits its diagnostics already name
//! (B-2026-11-03-10, found porting the mini_roaring app):
//!
//! * `==` / `!=` on a struct or enum of the program that does not derive
//!   `PartialEq` says "add #[derive(PartialEq)]"; the fix adds it, on its own
//!   line next to any derive the type already has, when every field compares, so the derive
//!   compiles as written. A type with a field that does not compare gets the
//!   message and no edit.
//! * `lossless_cast` on a dereference (`*v as i64` where an `i64` is wanted)
//!   deletes the `as`, as it already did on a name, a field or a call.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(name: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path
}

#[test]
fn karac_fix_derives_partial_eq_for_eq_operators() {
    let src = "enum Op {
    And,
    Or,
}

#[derive(Clone)]
struct Point {
    x: i64,
    y: i64,
}

fn pick(op: Op, a: bool, b: bool) -> bool {
    if op == Op.And { a and b } else { a or b }
}

fn main() {
    let p = Point { x: 1, y: 2 };
    let q = p.clone();
    println(f\"{pick(Op.And, true, false)} {pick(Op.Or, true, false)} {p != q}\");
}
";
    let path = temp("derive-partial-eq", src);
    let file = path.to_str().unwrap();

    let check = karac(&["check", file]);
    let err = String::from_utf8_lossy(&check.stderr);
    assert!(!check.status.success(), "{err}");
    for want in [
        "type 'Op' does not implement PartialEq; add #[derive(PartialEq)]",
        "type 'Point' does not implement PartialEq; add #[derive(PartialEq)]",
    ] {
        assert!(err.contains(want), "missing {want:?} in:\n{err}");
    }

    let fix = karac(&["fix", file]);
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(
        fix.status.success(),
        "{}\n{fixed}",
        String::from_utf8_lossy(&fix.stderr)
    );
    for want in [
        "#[derive(PartialEq)]\nenum Op {",
        "#[derive(Clone)]\n#[derive(PartialEq)]\nstruct Point {",
    ] {
        assert!(fixed.contains(want), "missing {want:?} in:\n{fixed}");
    }
    let recheck = karac(&["check", file]);
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert!(
        recheck.status.success(),
        "{}",
        String::from_utf8_lossy(&recheck.stderr)
    );
    assert_eq!(
        (
            run.status.code(),
            String::from_utf8_lossy(&run.stdout).as_ref()
        ),
        (Some(0), "false true false\n"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn karac_fix_offers_no_partial_eq_derive_a_field_would_refuse() {
    let src = "struct Inner {
    n: i64,
}

struct Outer {
    inner: Inner,
}

fn main() {
    let a = Outer { inner: Inner { n: 1 } };
    let b = Outer { inner: Inner { n: 1 } };
    println(f\"{a == b}\");
}
";
    let path = temp("derive-partial-eq-field", src);
    let file = path.to_str().unwrap();
    let json = karac(&["check", "--output=json", file]);
    let _ = std::fs::remove_file(&path);
    let json = String::from_utf8_lossy(&json.stdout);
    assert!(
        json.contains("type 'Outer' does not implement PartialEq"),
        "{json}"
    );
    assert!(!json.contains("#[derive(PartialEq)]\\n"), "{json}");
}

#[test]
fn karac_fix_deletes_an_unneeded_cast_on_a_deref() {
    let src = "fn wide(x: i64) -> i64 {
    x * 2
}

fn main() {
    let v: Vec[u16] = [3, 4];
    let mut t: i64 = 0;
    for x in v.iter() {
        t += wide(*x as i64);
    }
    println(f\"{t}\");
}
";
    let path = temp("deref-cast", src);
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    let err = String::from_utf8_lossy(&check.stderr);
    assert!(
        err.contains(":9:19: unnecessary cast: 'u16' widens to 'i64' implicitly here"),
        "{err}"
    );
    let fix = karac(&["fix", file]);
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    assert!(fixed.contains("t += wide(*x);"), "{fixed}");
    let recheck = karac(&["check", file]);
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert!(
        !String::from_utf8_lossy(&recheck.stderr).contains("_cast]"),
        "{}",
        String::from_utf8_lossy(&recheck.stderr)
    );
    assert_eq!(
        (
            run.status.code(),
            String::from_utf8_lossy(&run.stdout).as_ref()
        ),
        (Some(0), "14\n"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
