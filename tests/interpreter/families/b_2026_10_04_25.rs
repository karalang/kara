//! B-2026-10-04-25: a match arm binding over a borrowed `for` element, passed to a `ref` param, runs no Drop bodies in the interpreter

use super::*;

/// B-2026-10-04-25: the interpreter's arm stash asked only whether the
/// scrutinee was an owned or borrowed PARAM, so a `for` element the loop
/// borrows (`for item in items.iter()`) counted as consuming. An arm that
/// passed its binding on (`Many(xs) => count(xs)`, with `count` taking `ref`)
/// failed the read-through test and ran the payload's bodies at the arm's end,
/// once per call. The compiled backends run them once, when `items` dies
/// (after its last use, the second loop).
/// Covers `.iter()`, a bare place, `.enumerate()` and a top-level loop.
#[test]
fn interp_borrowed_loop_elem_arm_binding_runs_no_body() {
    let out = run(r#"struct D { n: i64 }
impl Drop for D { fn drop(mut ref self) { println(f"drop {self.n}"); } }
enum E { One(D), Many(Vec[D]) }
fn count(xs: ref Vec[D]) -> i64 { xs.len() }
fn one(d: ref D) -> i64 { d.n }
fn walk(items: ref Vec[E]) -> i64 {
    let mut total = 0;
    for item in items.iter() {
        match item {
            One(d) => { total += one(d); },
            Many(xs) => { total += count(xs); },
        }
    }
    total
}
fn walk_place(items: ref Vec[E]) -> i64 {
    let mut total = 0;
    for item in items {
        match item {
            One(d) => { total += one(d); },
            Many(xs) => { total += count(xs); },
        }
    }
    total
}
fn walk_enum(items: ref Vec[E]) -> i64 {
    let mut total = 0;
    for (i, item) in items.iter().enumerate() {
        match item {
            One(d) => { total += one(d) + i; },
            Many(xs) => { total += count(xs); },
        }
    }
    total
}
fn main() {
    let items = vec![Many(vec![D { n: 1 }, D { n: 2 }]), One(D { n: 3 })];
    println(f"walk {walk(items)}");
    println(f"walk {walk_place(items)}");
    println(f"walk {walk_enum(items)}");
    let mut t = 0;
    for item in items.iter() {
        match item {
            One(_) => { t += 1; },
            Many(xs) => { t += count(xs); },
        }
    }
    println(f"t {t}");
    println("end");
}
"#);
    assert_eq!(
        out,
        "walk 5\nwalk 5\nwalk 6\ndrop 1\ndrop 2\ndrop 3\nt 3\nend\n"
    );
}
