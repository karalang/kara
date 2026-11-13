//! B-2026-11-13-78: design.md § Default generic arguments: an argument
//! left out takes its parameter's default. A type written with no
//! arguments kept none, so `let a: Holder = Holder { v: 3 }` with
//! `struct Holder[T = i64]` was "expected 'Holder', found 'Holder[i64]'".
//! A bare type whose every parameter has a default now takes them all; a
//! path an expression calls through (`Holder.make(..)`) still infers.

use std::process::{Command, Stdio};

fn run(name: &str, src: &str) -> (bool, String, String) {
    let dir =
        std::env::temp_dir().join(format!("karac-default-bare-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let karac = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(args)
            .current_dir(&dir)
            .stdin(Stdio::null())
            .output()
            .expect("karac")
    };
    let check = karac(&["check", "cell.kara"]);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    let out = if check.status.success() {
        String::from_utf8_lossy(&karac(&["__mir-run", "cell.kara"]).stdout).into_owned()
    } else {
        String::new()
    };
    let _ = std::fs::remove_dir_all(&dir);
    (check.status.success(), err, out)
}

#[test]
fn bare_type_takes_every_default() {
    let (ok, err, out) = run(
        "annot",
        "struct Holder[T = i64] { v: T }\nfn main() { let a: Holder = Holder { v: 3 }; let b: Holder[String] = Holder { v: \"x\".to_string() }; println(f\"{a.v} {b.v}\"); }\n",
    );
    assert!(ok, "{err}");
    assert_eq!(out, "3 x\n");
}

#[test]
fn bare_type_in_a_signature_and_an_inferred_call_path() {
    let (ok, err, out) = run(
        "sig",
        "struct Holder[T = i64] { v: T }\n\
impl[T] Holder[T] { fn make(v: own T) -> Holder[T] { Holder { v: v } } }\n\
fn take(h: Holder) -> i64 { h.v }\n\
fn main() {\n    let a = Holder.make(\"s\".to_string());\n    let c: Holder = Holder.make(4);\n    println(f\"{a.v} {c.v} {take(c)}\");\n}\n",
    );
    assert!(ok, "{err}");
    assert_eq!(out, "s 4 4\n");
}

#[test]
fn bare_type_default_is_enforced() {
    let (ok, err, _) = run(
        "partial",
        "struct Holder[T = i64] { v: T }\nfn main() { let h: Holder = Holder { v: \"no\".to_string() }; }\n",
    );
    assert!(!ok);
    assert!(err.contains("expected 'i64', found 'String'"), "{err}");
}
