//! B-2026-11-19-18: a field typed `Option[weak P]` was accepted at the
//! definition, then every store into it was refused with a bare
//! "expected 'weak P', found 'P'". design.md § Cycles and `weak`: the
//! field is `weak P`, since emptiness is already a state of every weak
//! handle. The definition is now refused, naming the rule, with a
//! machine fix that drops the `Option[..]`; the fixed program checks and
//! runs. `Vec[weak P]` stays accepted.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(name: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("karac-b1918-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path
}

#[test]
fn option_of_weak_field_is_refused_and_fixed_to_weak() {
    let path = write(
        "opt",
        "shared struct P { v: i64 }
shared struct C { mut parent: Option[weak P] }
fn main() {
    let c = C { parent: None };
    let p = P { v: 3 };
    c.parent = Some(p);
    match c.parent { Some(q) => println(q.v), None => println(0) }
    c.parent = None;
    match c.parent { Some(q) => println(q.v), None => println(0) }
    println(p.v);
}
",
    );
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    let err = String::from_utf8_lossy(&check.stderr);
    assert!(
        err.contains("field `parent` is `Option[weak ..]`, but a weak handle is already optional"),
        "{err}"
    );
    karac(&["fix", file]);
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(
        fixed.contains("shared struct C { mut parent: weak P }"),
        "{fixed}"
    );
    let check = karac(&["check", file]);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "3\n0\n3\n");
}

#[test]
fn vec_of_weak_field_stays_accepted() {
    let path = write(
        "vec",
        "shared struct G { id: i64, mut out: Vec[weak G] }
fn main() {
    let a = G { id: 1, out: Vec.new() };
    let b = G { id: 2, out: Vec.new() };
    a.out.push(b);
    println(a.out.len());
    println(b.id);
}
",
    );
    let check = karac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
}
