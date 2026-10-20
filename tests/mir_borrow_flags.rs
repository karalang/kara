//! B-2026-10-19-55: compiled MIR keeps core semantics §6.2's run-time
//! borrow flags. Iterating `src.items` while pushing to `dst.items`, with
//! `src` and `dst` one object through two handles, panics as the MIR
//! interpreter does; the same loop over two objects, and repeated borrows
//! in a loop, run to completion.
//!
//! B-2026-10-20-14: the same holds when the loop suspends, so the borrow
//! is held by a coroutine frame across a yield.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const ALIAS: &str = r#"shared struct Bag { mut items: Vec[i64] }
fn add_all(src: Bag, dst: Bag) {
    for x in src.items {
        dst.items.push(x);
    }
}
fn main() {
    let b = Bag { items: Vec.new() };
    b.items.push(1);
    println("start");
    add_all(b, b);
    println("unreachable");
}
"#;

const APART: &str = r#"shared struct Bag { mut items: Vec[i64] }
fn add_all(src: Bag, dst: Bag) {
    for x in src.items {
        dst.items.push(x);
    }
}
fn total(b: Bag) -> i64 {
    let mut t = 0;
    for x in b.items { t = t + x; }
    t
}
fn main() {
    let a = Bag { items: Vec.new() };
    let b = Bag { items: Vec.new() };
    let mut i = 0;
    while i < 5 { a.items.push(i); i = i + 1; }
    add_all(a, b);
    add_all(a, b);
    println(f"{b.items.len()} {total(b)} {total(a)}");
}
"#;

const CO_ALIAS: &str = r#"shared struct Bag { mut items: Vec[i64] }
fn add_all(src: Bag, dst: Bag) {
    for x in src.items {
        sleep_ms(1);
        dst.items.push(x);
    }
}
fn main() {
    let b = Bag { items: Vec.new() };
    b.items.push(1);
    println("start");
    add_all(b, b);
    println("unreachable");
}
"#;

const CO_APART: &str = r#"shared struct Bag { mut items: Vec[i64] }
fn add_all(src: Bag, dst: Bag) {
    for x in src.items {
        sleep_ms(1);
        dst.items.push(x);
    }
}
fn main() {
    let a = Bag { items: Vec.new() };
    let b = Bag { items: Vec.new() };
    a.items.push(1);
    a.items.push(2);
    for _ in 0..3 { add_all(a, b); }
    b.items.push(9);
    add_all(b, a);
    println(f"{a.items.len()} {b.items.len()}");
}
"#;

/// The program's stdout and exit code, built ahead of time; `None` when
/// no runtime archive is there to link.
fn build_and_run(src: &str, tag: &str) -> Option<(String, Option<i32>)> {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::codegen::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = std::env::temp_dir().join(format!("karac-mir-flags-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::codegen::mir_aot::compile_object(&ir, &obj, 2).expect("object");
    let linked = common::link_or_skip(karac::codegen::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    let out = linked.map(|_| {
        let out = Command::new(&exe).output().expect("run");
        (String::from_utf8(out.stdout).unwrap(), out.status.code())
    });
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn a_shared_field_borrowed_through_another_handle_panics_when_compiled() {
    let interp = karac::mir::lower::run_source(ALIAS).expect("MIR interpreter");
    assert_eq!(interp.output, "start\n");
    if let Some((out, code)) = build_and_run(ALIAS, "alias") {
        assert_eq!(out, "start\n");
        assert_eq!(code, Some(101));
    }
}

#[test]
fn borrows_of_shared_fields_that_do_not_conflict_run_when_compiled() {
    let want = "10 20 10\n";
    let interp = karac::mir::lower::run_source(APART).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    if let Some((out, code)) = build_and_run(APART, "apart") {
        assert_eq!(out, want);
        assert_eq!(code, Some(0));
    }
}

#[test]
fn a_shared_field_borrowed_across_a_suspension_panics_when_compiled() {
    let interp = karac::mir::lower::run_source(CO_ALIAS).expect("MIR interpreter");
    assert_eq!(interp.output, "start\n");
    if let Some((out, code)) = build_and_run(CO_ALIAS, "co-alias") {
        assert_eq!(out, "start\n");
        assert_eq!(code, Some(101));
    }
}

#[test]
fn borrows_held_across_suspensions_are_given_back_when_compiled() {
    let want = "9 7\n";
    let interp = karac::mir::lower::run_source(CO_APART).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    if let Some((out, code)) = build_and_run(CO_APART, "co-apart") {
        assert_eq!(out, want);
        assert_eq!(code, Some(0));
    }
}
