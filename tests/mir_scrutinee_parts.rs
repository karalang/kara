//! B-2026-10-23-13: `match (a, b)` over borrowed parameters reads the
//! parameters part by part instead of copying each into a tuple, so a
//! handle bound out of a part counts nothing (kata 951's tree walk, 3.4x).
//! Bodies that write through a `shared` value, or bind the whole tuple,
//! keep the copies; all of them print what the interpreter prints.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"shared struct T { mut val: i64, left: Option[T], right: Option[T] }
fn leaf(v: i64) -> Option[T] { Some(T { val: v, left: None, right: None }) }
fn node(v: i64, l: Option[T], r: Option[T]) -> Option[T] { Some(T { val: v, left: l, right: r }) }
fn eqv(a: Option[T], b: Option[T]) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => x.val == y.val and eqv(x.left, y.left) and eqv(x.right, y.right),
        _ => false,
    }
}
fn plus(a: Option[T], n: i64) -> i64 {
    match (a, n) {
        (Some(x), 0) => x.val,
        (Some(x), k) => x.val + k,
        (None, k) => k,
    }
}
fn words(s: String, t: String) -> i64 {
    match (s, t) {
        (p, q) if p.len() == q.len() => 1,
        (p, _) if p.len() > 3 => 2,
        _ => 3,
    }
}
fn bump(a: Option[T], b: Option[T]) -> i64 {
    match (a, b) {
        (Some(x), Some(y)) => { x.val = x.val + y.val; x.val }
        _ => 0,
    }
}
fn whole(a: Option[T], b: Option[T]) -> i64 {
    match (a, b) {
        pair => match pair { (Some(x), _) => x.val, _ => -1 },
    }
}
fn main() {
    let a = node(1, leaf(2), node(3, None, leaf(4)));
    let b = node(1, leaf(2), node(3, None, leaf(4)));
    let c = node(1, leaf(2), node(3, leaf(4), None));
    println(f"{eqv(a, b)} {eqv(a, c)} {eqv(a, a)} {eqv(None, None)} {eqv(a, None)}");
    println(f"{plus(a, 0)} {plus(a, 5)} {plus(None, 7)}");
    println(f"{words("ab", "cd")} {words("abcd", "x")} {words("a", "xyz")}");
    println(f"{bump(a, b)} {bump(a, b)} {bump(None, b)}");
    println(f"{whole(a, b)} {whole(None, b)}");
    println(f"{eqv(a, b)}");
}
"#;

const WANT: &str = "true false true true false\n1 6 7\n1 2 3\n2 3 0\n3 -1\nfalse\n";

fn ir(src: &str) -> String {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR")
}

fn compiled(ir: &str, opt: u8) -> Option<String> {
    let dir = std::env::temp_dir().join(format!(
        "karac-mir-scrutinee-parts-{}-{opt}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(ir, &obj, opt).expect("object");
    let linked = common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    let out = linked.map(|_| Command::new(&exe).output().expect("run"));
    let _ = std::fs::remove_dir_all(&dir);
    let out = out?;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8(out.stdout).expect("utf-8"))
}

/// The body of `define ... @<name>(`, up to its closing brace.
fn body_of<'a>(ir: &'a str, name: &str) -> &'a str {
    let head = format!("@{name}(");
    let start = ir
        .lines()
        .position(|l| l.starts_with("define") && l.contains(&head))
        .unwrap_or_else(|| panic!("no define of {name}"));
    let from: usize = ir.lines().take(start).map(|l| l.len() + 1).sum();
    let rest = &ir[from..];
    &rest[..rest.find("\n}").expect("end of body")]
}

#[test]
fn mir_tuple_scrutinee_reads_borrowed_parameters() {
    let want = karac::mir::lower::run_source(SRC)
        .expect("MIR interpreter")
        .output;
    assert_eq!(want, WANT);
    let ir = ir(SRC);
    let eqv = body_of(&ir, "kara.eqv");
    // No counted copy of a part, and no drop of the tuple or a handle.
    for call in ["@\"clone.", "@\"drop.", "@\"last."] {
        assert!(!eqv.contains(call), "eqv calls {call}:\n{eqv}");
    }
    for opt in [0, 2] {
        if let Some(out) = compiled(&ir, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}
