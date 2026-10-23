//! Bodies whose parameters and result fit registers are called by value
//! (`Cx::value_sig`), and a handle a body only reads out of a `ref`
//! parameter is borrowed rather than counted (`mir::rc_borrow`): a
//! recursive tree walk (kata 101) and a handle that leaves the body (which
//! keeps its count) print what the interpreter prints, and the walk's
//! function takes its arguments in registers.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"shared struct T { val: i64, mut left: Option[T], mut right: Option[T] }
fn leaf(v: i64) -> Option[T] { Some(T { val: v, left: None, right: None }) }
fn is_mirror(a: Option[T], b: Option[T]) -> bool {
    match a {
        None => { match b { None => true, Some(_) => false } }
        Some(an) => { match b { None => false, Some(bn) => an.val == bn.val and is_mirror(an.left, bn.right) and is_mirror(an.right, bn.left) } }
    }
}
fn first(a: Option[T]) -> T {
    match a {
        Some(x) => x,
        None => T { val: 0, left: None, right: None }
    }
}
fn cut(h: T) { h.left = None; }
fn peek(a: Option[T], h: T) -> i64 {
    match a {
        Some(x) => { cut(h); x.val }
        None => 0
    }
}
fn fib(n: i64) -> i64 { if n < 2 { return n; } fib(n - 1) + fib(n - 2) }
fn main() {
    let l = T { val: 1, left: leaf(2), right: leaf(3) };
    let r = T { val: 1, left: leaf(3), right: leaf(2) };
    let root = T { val: 0, left: Some(l), right: Some(r) };
    println(is_mirror(root.left, root.right));
    println(is_mirror(root.left, root.left));
    let f = first(root.left);
    root.left = None;
    println(f.val);
    println(peek(f.left, f));
    println(fib(20));
}
"#;

const WANT: &str = "true\nfalse\n1\n2\n6765\n";

#[test]
fn mir_byvalue_calls_and_borrowed_handles_on_both_backends() {
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, WANT);
    let lowered = karac::mir::lower::build_source(SRC).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    assert!(
        ir.contains("define internal i8 @kara.is_mirror(ptr"),
        "is_mirror is not called by value:\n{ir}"
    );
    let dir = std::env::temp_dir().join(format!("karac-mir-byvalue-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 2).expect("object");
    let linked = common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    if linked.is_some() {
        let out = Command::new(&exe).output().expect("run");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8(out.stdout).unwrap(), WANT);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
