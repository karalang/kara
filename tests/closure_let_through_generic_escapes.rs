//! B-2026-11-20-5: a closure literal handed through a generic identity on
//! `own F` and bound by a `let` that is then returned escapes as the same
//! value returned directly does (core semantics §9.3), so it captures by
//! move. It was refused E0509 ("the result borrows `s`, which this
//! function owns"), while the tail `keep(|| s.len())` and `let f = ||
//! s.len(); f` were accepted.

use std::process::{Command, Output, Stdio};

const SRC: &str = r#"struct D { n: String }
impl Drop for D { fn drop(mut ref self) { println("drop D"); } }
fn keep[F: Fn() -> i64](f: own F) -> F { f }
fn mk() -> Fn() -> i64 { let s = "abc".to_string(); let f = keep(|| s.len()); f }
fn mk2() -> Fn() -> i64 { let d = D { n: "hello".to_string() }; let f = keep(|| d.n.len()); f }
fn main() {
    let f = mk();
    println(f());
    let g = mk2();
    println(g());
}
"#;

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

#[test]
fn a_let_bound_closure_through_a_generic_identity_escapes_by_move() {
    let path = std::env::temp_dir().join(format!(
        "karac-let-through-generic-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, SRC).unwrap();
    let check = karac(&["check"], &path);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let run = karac(&["__mir-run"], &path);
    let _ = std::fs::remove_file(&path);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "3\n5\ndrop D\n");
}
