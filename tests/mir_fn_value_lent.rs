//! An erased function value (a struct field's, a function's result) passed
//! to a parameter that borrows it (a bare `Fn`, core semantics §9.3) is
//! lent, not moved: the same value goes to two calls, and to a callee that
//! passes it on twice. An `own` parameter still takes it. On the MIR
//! interpreter, the built binary and ASan. B-2026-10-27-12 (the builder's
//! half; a `Vec` element waits on check's).
#![cfg(feature = "llvm")]

mod common;

use std::process::{Command, Output};

const SRC: &str = r#"fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }
fn bump(f: mut ref MutFn(i64) -> i64, x: i64) -> i64 { f(x) + f(x) }
fn pick(s: own String) -> Fn(i64) -> i64 { |x: i64| x + s.len() as i64 }
fn counter() -> MutFn(i64) -> i64 {
    let mut n = 0;
    |x: i64| { n += 1; x + n }
}
fn twice(f: Fn(i64) -> i64, x: i64) -> i64 {
    apply(f, x) + apply(f, x + 1)
}
struct Job { run: Fn(i64) -> i64 }
fn main() {
    let j = Job { run: pick(String.from("ab")) };
    println(apply(j.run, 1));
    println(apply(j.run, 2));
    let g = pick(String.from("abc"));
    println(apply(g, 1));
    println(twice(g, 10));
    let mut c = counter();
    println(bump(mut c, 10));
    println(bump(mut c, 10));
    last();
}
fn consume(f: own MutFn(i64) -> i64) -> i64 { f(1) + f(1) }
fn last() { let c = counter(); println(consume(c)); }
"#;

const OUT: &str = "3\n4\n4\n27\n23\n27\n5\n";

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn mir_an_erased_fn_value_is_lent_to_a_borrowing_parameter() {
    let dir = std::env::temp_dir().join(format!("karac-fn-value-lent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let karac = || Command::new(env!("CARGO_BIN_EXE_karac"));
    let out = karac()
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), OUT);

    let exe = dir.join("main");
    let built = karac()
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr).to_string();
    assert!(
        built.status.success() || err.contains("Linker failed"),
        "does not build: {err}"
    );
    if common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(err)
    })
    .is_some()
    {
        let ran = Command::new(&exe).output().unwrap();
        assert!(ran.status.success(), "{}", text(&ran));
        assert_eq!(String::from_utf8_lossy(&ran.stdout), OUT);
        let asan = karac()
            .args(["__mir-llvm-asan", file.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(asan.status.success(), "{}", text(&asan));
        assert_eq!(String::from_utf8_lossy(&asan.stdout), OUT);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
