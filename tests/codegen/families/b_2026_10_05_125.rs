//! B-2026-10-05-125 — reassigning a local releases the `shared` value it displaced.

use super::*;

/// B-2026-10-05-125 — reassigning a local runs the `Drop` body of the
/// `shared` value it displaced, at the store: a bare handle (`a`, `e`, `k`,
/// `p`, `q`), an `Option` payload (`b`, `c`, `o`), a struct field (`g`, `l`,
/// `m`), an enum payload (`i`, `j`) and a `Vec` element (`n`). A handle
/// aliased elsewhere (`d`) stays alive until its last holder dies.
///
/// Before: the interpreter ran none of these bodies at the store, and lost
/// most of them outright; compiled code was right throughout.
#[test]
fn e2e_reassign_releases_displaced_shared() {
    let src = r#"shared struct H { id: i64 }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct S { h: H, n: i64 }
struct Sr { r: R, h: H }
struct Hr { h: H, r: R }
enum E { A(H), B }
fn mkh(i: i64) -> H { H { id: i } }
fn take(h: H) { println(f"t{h.id}") }
fn a() { let mut h = H { id: 1 }; h = H { id: 2 }; println(f"a{h.id}") }
fn b() { let mut o = Some(H { id: 3 }); o = Some(H { id: 4 }); println(f"b{o.is_some()}") }
fn c() { let mut o = Some(H { id: 5 }); o = None; println(f"c{o.is_some()}") }
fn d() { let mut h = H { id: 6 }; let g = h; h = H { id: 7 }; println(f"d{h.id} {g.id}") }
fn e() { let mut h = mkh(8); h = mkh(9); println(f"e{h.id}") }
fn f() { let mut h = H { id: 10 }; take(h); h = H { id: 11 }; println(f"f{h.id}") }
fn g() { let mut s = S { h: H { id: 12 }, n: 0 }; s = S { h: H { id: 13 }, n: 1 }; println(f"g{s.n}") }
fn i() { let mut e = E.A(H { id: 14 }); e = E.A(H { id: 15 }); match e { E.A(x) => println(f"i{x.id}"), E.B => println("i") } }
fn j() { let mut e = E.A(H { id: 16 }); e = E.B; println("j") }
fn k() { let mut h = H { id: 17 }; let mut n = 0; while n < 2 { h = H { id: n + 18 }; n = n + 1; } println(f"k{h.id}") }
fn l() { let mut s = Sr { r: R { id: 20 }, h: H { id: 21 } }; s = Sr { r: R { id: 22 }, h: H { id: 23 } }; println(f"l{s.r.id}") }
fn m() { let mut s = Hr { h: H { id: 24 }, r: R { id: 25 } }; s = Hr { h: H { id: 26 }, r: R { id: 27 } }; println(f"m{s.r.id}") }
fn n() { let mut v = [H { id: 28 }]; v = [H { id: 29 }]; println(f"n{v.len()}") }
fn o() { let mut o: Option[H] = None; o = Some(H { id: 30 }); o = Some(H { id: 31 }); println(f"o{o.is_some()}") }
fn p() { let mut h = H { id: 32 }; if true { h = H { id: 33 }; } println(f"p{h.id}") }
fn q() { let a = H { id: 34 }; let mut c = a; c = H { id: 35 }; println(f"q{c.id}") }
fn main() {
    a()
    b()
    c()
    d()
    e()
    f()
    g()
    i()
    j()
    k()
    l()
    m()
    n()
    o()
    p()
    q()
    println("end")
}
"#;
    let want = "dH1\na2\ndH2\ndH3\nbtrue\ndH4\ndH5\ncfalse\nd7 6\ndH6\ndH7\ndH8\ne9\ndH9\nt10\ndH10\nf11\ndH11\ndH12\ng1\ndH13\ndH14\ni15\ndH15\ndH16\nj\ndH17\ndH18\nk19\ndH19\ndR20\ndH21\nl22\ndR22\ndH23\ndR25\ndH24\nm27\ndR27\ndH26\ndH28\nn1\ndH29\ndH30\notrue\ndH31\ndH32\np33\ndH33\ndH34\nq35\ndH35\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
