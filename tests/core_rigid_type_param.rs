//! Inside a generic item its own type parameter is rigid (B-2026-10-27-19):
//! only a `T` is a `T`, so `karac check` refuses a concrete value where the
//! item's `T` is expected, and a `T` where a concrete type is. Before, the
//! `TypeParam` wildcard of `types_compatible` let these through to the MIR
//! validator ("invalid MIR: f[String]: assigning i64 to _0 of type String").

use std::path::PathBuf;
use std::process::{Command, Output};

fn write(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-rigid-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn check(tag: &str, src: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(write(tag, src))
        .output()
        .unwrap()
}

fn refused(tag: &str, src: &str, expect: &str) {
    let out = check(tag, src);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{tag}: check must refuse:\n{err}");
    assert!(
        err.contains(expect),
        "{tag}: expected `{expect}` in:\n{err}"
    );
}

fn accepted(tag: &str, src: &str) {
    let out = check(tag, src);
    assert!(
        out.status.success(),
        "{tag}: check must accept:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn rigid_param_return_of_a_literal_refused() {
    refused(
        "ret-literal",
        "fn f[T](x: own T) -> T { 5 }\nfn main() { println(f(\"a\")); }\n",
        "expected 'T', found 'i64': inside this item `T` stands for whatever type the caller picks",
    );
}

#[test]
fn rigid_param_as_a_concrete_return_refused() {
    refused(
        "ret-param",
        "fn f[T](x: own T) -> i64 { x }\nfn main() { println(f(\"a\")); }\n",
        "expected 'i64', found 'T'",
    );
}

#[test]
fn rigid_param_inside_a_type_refused() {
    refused(
        "vec",
        "fn g[T](x: own T) -> Vec[T] { vec![1, 2] }\nfn main() { println(g(\"a\").len()); }\n",
        "expected 'Vec[T]', found 'Vec[i64]'",
    );
}

#[test]
fn rigid_param_let_annotations_refused() {
    refused(
        "let-into",
        "fn h[T](x: own T) {\n    let y: T = 3;\n}\nfn main() { h(1); }\n",
        "expected 'T', found 'i64'",
    );
    refused(
        "let-from",
        "fn h[T](x: own T) {\n    let z: i64 = x;\n}\nfn main() { h(1); }\n",
        "expected 'i64', found 'T'",
    );
}

/// A parameter a callee leaves unsolved (`None`'s `T`, `Vec.new()`'s) is
/// not the enclosing item's, even when both are named `T`.
#[test]
fn rigid_param_leaves_unsolved_callee_params_alone() {
    accepted(
        "callee-params",
        "fn f[T](x: own T) -> Option[i64] { None }\n\
         fn g[T](x: own T) -> Option[T] { None }\n\
         fn h[T: Clone](x: ref T) -> Vec[T] {\n    let mut v = Vec.new();\n    v.push(x.clone());\n    v\n}\n\
         fn k[T](x: own T) -> T { x }\n\
         fn main() {\n    println(f(\"a\").is_none());\n    println(g(1).is_none());\n    println(h(2).len());\n    println(k(3));\n}\n",
    );
}
