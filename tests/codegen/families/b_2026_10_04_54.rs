//! B-2026-10-04-54 -- `.clone()` on a tuple element whose tuple is not a
//! named owned local (an indexed element of a `Vec` / `Array` / slice of
//! tuples, or a tuple behind a `ref` / `mut ref` root) failed the build.

use super::*;

/// `String`, `Vec[String]` and derive-`Clone` struct elements cloned out of
/// indexed tuples and borrowed tuples; each clone is independent of its
/// source.
#[test]
fn e2e_clone_of_indexed_or_borrowed_tuple_element() {
    let src = r#"#[derive(Clone)]
struct Q { s: String, k: i64 }
fn f(p: ref (Vec[String], i64)) -> Vec[String] { return p.0.clone() }
fn g(p: mut ref (String, i64)) -> String { let s = p.0.clone(); p.0 = f"z{9}"; return s }
fn h(xs: ref Vec[(String, i64)]) -> String { return xs[0].0.clone() }
fn k(xs: Slice[(String, i64)]) -> String { return xs[1].0.clone() }
fn r(p: ref (String, i64)) -> String { return p.0.clone() }
fn main() {
    let cases: Vec[(String, i64)] = [(f"ab{1}", 1), (f"cd{2}", 2), (f"ef{3}", 3)];
    let mut out: Vec[String] = Vec.new();
    let mut i = 0;
    while i < cases.len() { let s = cases[i].0.clone(); out.push(s); i += 1; }
    println(f"a:{out[2]} {out.len()} {cases[0].0}");
    let qs: Vec[(Q, i64)] = [(Q { s: f"q{1}", k: 4 }, 5)];
    let q = qs[0].0.clone();
    println(f"b:{q.s} {q.k}");
    let arr: Array[(String, i64), 2] = [(f"x{1}", 1), (f"y{2}", 2)];
    let y = arr[1].0.clone();
    println(f"c:{y} {arr[1].0}");
    let vv: Vec[(Vec[String], i64)] = [([f"m{1}", f"n{2}"], 0)];
    let c = vv[0].0.clone();
    println(f"d:{c[1]} {c.len()}");
    let p: (Vec[String], i64) = ([f"a{1}"], 1);
    let pc = f(p);
    let mut m: (String, i64) = (f"b{2}", 2);
    let s = g(mut m);
    println(f"e:{pc[0]} {p.0[0]} {s} {m.0} {h(cases)} {k(cases)} {r(m)}");
}
"#;
    let want = "a:ef3 3 ab1\nb:q1 4\nc:y2 y2\nd:n2 2\ne:a1 a1 b2 z9 ab1 cd2 z9\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
