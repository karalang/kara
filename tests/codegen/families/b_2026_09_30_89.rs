//! B-2026-09-30-89 / B-2026-10-04-39 -- a tuple reached through a `ref` /
//! `mut ref` root (a tuple parameter, or a tuple field of `self`) had no place
//! resolver, so reads, stores and methods on its elements failed the build.

use super::*;

/// `ref` and `mut ref` tuple parameters: index reads, nested tuple reads,
/// field, element and whole-element stores, and `Vec` methods on an element,
/// all landing in the caller's tuple.
#[test]
fn e2e_borrowed_tuple_param_element_places() {
    let src = r#"struct P { n: i64 }
struct Q { s: String, k: i64 }
fn rd(a: ref (Array[P, 2], i64)) -> i64 { return a.0[1].n + a.1 }
fn rv(a: ref (Vec[String], i64)) -> String { let x = a.0[1].clone(); return f"{x}{a.0.len()}" }
fn rn(a: ref ((Vec[i64], i64), i64)) -> i64 { return a.0.0[1] + a.0.1 + a.1 }
fn wp(a: mut ref (Array[P, 2], i64)) { a.0[0].n = 9; a.0[1] = P { n: 8 }; a.1 = 3; }
fn ws(a: mut ref (Array[String, 2], i64)) { a.0[1] = f"z{9}"; }
fn wq(a: mut ref (Array[Q, 2], i64)) { a.0[1] = Q { s: f"q{7}", k: 3 }; a.0[0].s = f"w{8}"; }
fn wt(a: mut ref (String, Q, Vec[String])) { a.0 = f"n{1}"; a.1 = Q { s: f"q{2}", k: 3 }; a.1.s = f"r{4}"; a.2[0] = f"v{5}"; a.2.push(f"w{6}"); }
fn wv(a: mut ref (Vec[i64], i64)) { a.0[1] = 40; a.0.push(3); a.1 = a.0.len(); }
fn main() {
    let mut p: (Array[P, 2], i64) = ([P { n: 5 }, P { n: 6 }], 7);
    println(f"a:{rd(p)}");
    wp(mut p);
    println(f"b:{p.0[0].n} {p.0[1].n} {p.1} {rd(p)}");
    let v: (Vec[String], i64) = ([f"x{1}", f"y{2}"], 7);
    println(f"c:{rv(v)}");
    let n: ((Vec[i64], i64), i64) = (([1, 2], 3), 4);
    println(f"d:{rn(n)}");
    let mut s: (Array[String, 2], i64) = ([f"x{1}", f"y{2}"], 7);
    ws(mut s);
    println(f"e:{s.0[0]} {s.0[1]}");
    let mut q: (Array[Q, 2], i64) = ([Q { s: f"x{1}", k: 1 }, Q { s: f"y{2}", k: 2 }], 7);
    wq(mut q);
    println(f"f:{q.0[0].s} {q.0[1].s} {q.0[1].k}");
    let mut t: (String, Q, Vec[String]) = (f"o{0}", Q { s: f"p{0}", k: 0 }, [f"u{0}"]);
    wt(mut t);
    println(f"g:{t.0} {t.1.s} {t.1.k} {t.2[0]} {t.2[1]}");
    let mut w: (Vec[i64], i64) = ([1, 2], 7);
    wv(mut w);
    println(f"h:{w.0[1]} {w.0[2]} {w.1}");
}
"#;
    let want = "a:13\nb:9 8 3 11\nc:y22\nd:9\ne:x1 z9\nf:w8 q7 3\ng:n1 r4 3 v5 w6\nh:40 3 3\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// The same places under `ref self` / `mut ref self`, through a tuple field.
#[test]
fn e2e_mut_ref_self_tuple_field_element_places() {
    let src = r#"struct P { n: i64 }
struct H { t: (Array[P, 2], i64), s: (Array[String, 2], i64), v: (Vec[String], i64) }
impl H {
    fn get(ref self) -> String { return f"{self.t.0[1].n} {self.t.1} {self.s.0[0]} {self.v.0[0]} {self.v.0.len()}" }
    fn set(mut ref self, s: String) {
        self.t.0[1].n = 40;
        self.t.1 = 2;
        self.t.0[0] = P { n: 11 };
        self.s.0[0] = s;
        self.v.0.push(f"c{3}");
        self.v.1 = self.v.0.len();
    }
}
fn main() {
    let mut h = H { t: ([P { n: 1 }, P { n: 2 }], 3), s: ([f"h{1}", f"i{2}"], 0), v: ([f"a{1}"], 0) };
    println(f"a:{h.get()}");
    h.set(f"k{4}");
    println(f"b:{h.get()} {h.t.0[0].n} {h.v.1}");
}
"#;
    let want = "a:2 3 h1 a1 1\nb:40 2 k4 a1 2 11 2\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// A field store into a `Vec` element held in a borrowed tuple, through a
/// `mut ref` parameter (pure and impure index, compound assignment) and through
/// `mut ref self`.
#[test]
fn e2e_field_store_into_vec_element_of_borrowed_tuple() {
    let src = r#"struct P { n: i64, s: String }
struct H { t: (Vec[P], i64) }
impl H { fn set(mut ref self) { self.t.0[1].n = 40; self.t.0[0].s = f"h{9}"; } }
fn g() -> i64 { println("g"); return 1 }
fn f(a: mut ref (Vec[P], i64)) { a.0[0].n = 9; a.0[g()].s = f"t{3}"; a.0[1].n += 4; }
fn main() {
    let mut a: (Vec[P], i64) = ([P { n: 5, s: f"x{1}" }, P { n: 6, s: f"y{2}" }], 7);
    f(mut a);
    println(f"a:{a.0[0].n} {a.0[1].n} {a.0[1].s}");
    let mut h = H { t: ([P { n: 1, s: f"u{1}" }, P { n: 2, s: f"v{2}" }], 3) };
    h.set();
    println(f"b:{h.t.0[1].n} {h.t.0[0].s}");
}
"#;
    let want = "g\na:9 10 t3\nb:40 h9\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
