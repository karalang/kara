//! B-2026-10-05-119: a fresh temporary holding a `shared` value, passed by value, releases it once

use super::*;

/// B-2026-10-05-119 — a fresh `Option`, enum, `Vec` or tuple temporary
/// holding a `shared` value, passed by value to a callee that keeps nothing.
/// The interpreter never ran the handle's `Drop` body (`co(Some(H { id: 1 }))`
/// printed `o _1`), and a tuple temp lost it on every surface and leaked it
/// compiled. Each body now runs once where the caller's temporary dies, for a
/// free fn and a method, `Result` on either side, a nest, a generic enum, a
/// tuple from a call, and a struct payload; a callee that hands the value
/// back or stores it keeps its body for the new owner.
#[test]
fn e2e_fresh_temp_holding_shared_value_passed_by_value_releases_it_once() {
    let Some(out) = run_program(
        r#"shared struct H { id: i64 }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum E { A(H), B }
enum G[T] { Gx(T), Gy }
struct X { h: H, n: i64 }
struct K { n: i64 }
impl K {
    fn mo(ref self, o: Option[H]) { println("mo") }
    fn me(ref self, e: E) { println("me") }
    fn mt(ref self, t: (H, i64)) { println("mt") }
    fn mv(ref self, v: Vec[H]) { println("mv") }
}
fn co(o: Option[H]) { println("o") }
fn ce(e: E) { println("e") }
fn cv(v: Vec[H]) { println("v") }
fn ct(t: (H, i64)) { println("t") }
fn ct2(t: (H, R)) { println("t2") }
fn cg(g: G[H]) { println("g") }
fn ov(o: Option[Vec[H]]) { println("ov") }
fn oe(o: Option[E]) { println("oe") }
fn ox(o: Option[X]) { println("ox") }
fn oo(o: Option[Option[H]]) { println("oo") }
fn rr(o: Result[H, i64]) { println("rr") }
fn re(o: Result[i64, H]) { println("re") }
fn vt(v: Vec[(H, i64)]) { println("vt") }
fn ve(v: Vec[E]) { println("ve") }
fn eo(e: Option[H], f: Option[H]) { println("eo") }
fn mkt(i: i64) -> (H, i64) { (H { id: i }, i) }
fn ko(o: Option[H]) -> Option[H] { o }
fn ke(e: E) -> E { e }
fn kv(v: Vec[H]) -> Vec[H] { v }
fn kt(t: (H, i64)) -> (H, i64) { t }
fn rt(t: (H, i64)) -> i64 { t.1 }
fn so(o: Option[H], d: mut ref Vec[Option[H]]) { d.push(o) }
fn st(t: (H, i64), d: mut ref Vec[(H, i64)]) { d.push(t) }
fn coc(o: Option[H], c: bool) -> Option[H] { if c { return o } None }
fn c1() { co(Some(H { id: 1 })); println("_1"); }
fn c2() { ce(E.A(H { id: 2 })); println("_2"); }
fn c3() { cv(vec![H { id: 3 }]); println("_3"); }
fn c4() { ct((H { id: 4 }, 4)); println("_4"); }
fn c5() { ct(mkt(5)); println("_5"); }
fn c6() { ct2((H { id: 6 }, R { id: 6 })); println("_6"); }
fn c7() { println(f"_7 {rt((H { id: 7 }, 7))}"); }
fn c8() { cg(G.Gx(H { id: 8 })); println("_8"); }
fn c9() { let k = K { n: 0 }; k.mo(Some(H { id: 9 })); println("_9"); }
fn c10() { let k = K { n: 0 }; k.me(E.A(H { id: 10 })); println("_10"); }
fn c11() { let k = K { n: 0 }; k.mt((H { id: 11 }, 11)); println("_11"); }
fn c12() { let k = K { n: 0 }; k.mv(vec![H { id: 12 }]); println("_12"); }
fn c13() { ov(Some(vec![H { id: 13 }])); println("_13"); }
fn c14() { oe(Some(E.A(H { id: 14 }))); println("_14"); }
fn c15() { ox(Some(X { h: H { id: 15 }, n: 15 })); println("_15"); }
fn c16() { oo(Some(Some(H { id: 16 }))); println("_16"); }
fn c17() { rr(Ok(H { id: 17 })); println("_17"); }
fn c18() { re(Err(H { id: 18 })); println("_18"); }
fn c19() { vt(vec![(H { id: 19 }, 19)]); println("_19"); }
fn c20() { ve(vec![E.A(H { id: 20 })]); println("_20"); }
fn c21() { eo(Some(H { id: 21 }), Some(H { id: 22 })); println("_21"); }
fn c22() { let o = ko(Some(H { id: 23 })); println(f"_23 {o.is_some()}"); }
fn c23() { let e = ke(E.A(H { id: 24 })); println("_24"); }
fn c24() { let v = kv(vec![H { id: 25 }]); println(f"_25 {v.len()}"); }
fn c25() { let t = kt((H { id: 26 }, 26)); println(f"_26 {t.1}"); }
fn c26() { let mut d: Vec[Option[H]] = Vec.new(); so(Some(H { id: 27 }), mut d); println(f"_27 {d.len()}"); }
fn c27() { let mut d: Vec[(H, i64)] = Vec.new(); st((H { id: 28 }, 28), mut d); println(f"_28 {d.len()}"); }
fn c28() { let p = coc(Some(H { id: 29 }), true); println(f"_29 {p.is_some()}"); }
fn main() {
    c1(); c2(); c3(); c4(); c5(); c6(); c7(); c8(); c9(); c10(); c11(); c12(); c13(); c14();
    c15(); c16(); c17(); c18(); c19(); c20(); c21(); c22(); c23(); c24(); c25(); c26(); c27(); c28();
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "o\ndH1\n_1\ne\ndH2\n_2\nv\ndH3\n_3\nt\ndH4\n_4\nt\ndH5\n_5\nt2\ndR6\ndH6\n_6\n_7 7\ndH7\ng\ndH8\n_8\nmo\ndH9\n_9\nme\ndH10\n_10\nmt\ndH11\n_11\nmv\ndH12\n_12\nov\ndH13\n_13\noe\ndH14\n_14\nox\ndH15\n_15\noo\ndH16\n_16\nrr\ndH17\n_17\nre\ndH18\n_18\nvt\ndH19\n_19\nve\ndH20\n_20\neo\ndH22\ndH21\n_21\n_23 true\ndH23\n_24\ndH24\n_25 1\ndH25\n_26 26\ndH26\n_27 1\ndH27\n_28 1\ndH28\n_29 true\ndH29\nend\n", "got:\n{out}");
}
