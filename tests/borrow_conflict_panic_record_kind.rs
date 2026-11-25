//! B-2026-11-25-25, design.md § Panic record: a conflicting access to a
//! `shared` field through another handle (core semantics §6.2) is recorded
//! with kind `borrow_conflict`, on the MIR interpreter and the JIT alike;
//! the message and place are `tests/borrow_flag_conflict_names_both.rs`'s.

use std::process::{Command, Stdio};

const ALIAS: &str = "shared struct Bag { mut items: Vec[i64] }
fn add_all(src: Bag, dst: Bag) {
    for x in src.items {
        dst.items.push(x);
    }
}
fn main() {
    let b = Bag { items: Vec.new() };
    b.items.push(1);
    add_all(b, b);
}
";

/// `karac run --output=json` (with `extra`): the exit code and the
/// runtime-error record's text.
fn record(tag: &str, extra: &[&str]) -> (Option<i32>, String, String) {
    let dir =
        std::env::temp_dir().join(format!("karac-conflict-kind-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("m.kara"), ALIAS).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(["run", "--output=json"])
        .args(extra)
        .arg("m.kara")
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn assert_borrow_conflict(code: Option<i32>, out: &str, err: &str) {
    assert_eq!(code, Some(101), "{out}{err}");
    assert!(out.contains("already borrowed: `Bag.items`"), "{out}{err}");
    assert!(
        out.contains("\"kind\":\"borrow_conflict\""),
        "the record's kind: {out}{err}"
    );
}

#[test]
fn a_shared_field_conflict_is_kind_borrow_conflict_on_the_interpreter() {
    let (code, out, err) = record("interp", &["--interp"]);
    assert_borrow_conflict(code, &out, &err);
}

#[cfg(feature = "llvm")]
#[test]
fn a_shared_field_conflict_is_kind_borrow_conflict_on_the_jit() {
    let (code, out, err) = record("jit", &[]);
    if code != Some(101)
        && err.contains("archive")
        && std::env::var("KARAC_REQUIRE_RUNTIME_ARCHIVE").as_deref() != Ok("1")
    {
        eprintln!("skipped: {err}");
        return;
    }
    assert_borrow_conflict(code, &out, &err);
}
