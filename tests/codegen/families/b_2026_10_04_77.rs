//! B-2026-10-04-77 -- a `for` binding over a fixed `Array` of `String` or
//! `Vec` elements could not take a method call, and moving a `for` binding
//! out of any heap-element `Array` handed the array's buffers to a second
//! owner.

use super::*;

/// Methods on `String` and `Vec` loop bindings over a local array, a `ref`
/// param, `ref self`'s field and a tuple element; moves out of the binding
/// copy rather than alias.
#[test]
fn e2e_array_for_binding_methods_and_moves() {
    let src = r#"struct P { n: i64, s: String }
struct H { names: Array[String, 2] }
impl H { fn total(ref self) -> i64 { let mut k = 0; for x in self.names { k += x.len(); } return k } }
fn cnt(a: ref Array[Vec[i64], 2]) -> i64 { let mut k = 0; for v in a { k += v.len(); if v.contains(3) { k += 100; } } return k }
fn main() {
    let ss: Array[String, 2] = [f"q{1}", f"rr{2}"];
    let mut out: Vec[String] = Vec.new();
    for x in ss { out.push(x); let up = x.to_uppercase(); println(f"{up} {x.len()} {x.starts_with("q")}"); }
    println(f"a:{out[0]}{out[1]} {ss[0]}");
    let h = H { names: [f"ab{1}", f"c{2}"] };
    println(f"b:{h.total()}");
    let vv: Array[Vec[i64], 2] = [[1, 2], [3]];
    println(f"c:{cnt(vv)}");
    let t: (Array[String, 2], i64) = ([f"x{1}", f"yy{2}"], 3);
    let mut m = 0;
    for s in t.0 { m += s.len(); if s.ends_with("2") { println(f"d:{s}"); } }
    println(f"e:{m} {t.0[1]}");
    let ps: Array[P, 2] = [P { n: 1, s: f"p{1}" }, P { n: 2, s: f"o{2}" }];
    let mut kept: Vec[P] = Vec.new();
    for p in ps { kept.push(p); }
    for p in ps { let q = p; println(f"f:{q.n}{q.s}"); }
    println(f"g:{kept[1].s} {ps[1].s} {kept.len()}");
}
"#;
    let want =
        "Q1 2 true\nRR2 3 false\na:q1rr2 q1\nb:5\nc:103\nd:yy2\ne:5 yy2\nf:1p1\nf:2o2\ng:o2 o2 2\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
