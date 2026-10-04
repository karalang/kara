//! B-2026-10-04-54 -- a clone of an indexed or borrowed tuple element is
//! a fresh owner: freed once, and its source freed once.

use super::*;

/// The clones and their sources each freed exactly once.
#[test]
fn asan_clone_of_indexed_or_borrowed_tuple_element() {
    assert_clean_asan_run(
        r#"#[derive(Clone)]
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
"#,
        &[
            "a:ef3 3 ab1",
            "b:q1 4",
            "c:y2 y2",
            "d:n2 2",
            "e:a1 a1 b2 z9 ab1 cd2 z9",
        ],
        "clone_of_indexed_or_borrowed_tuple_element",
    );
}
