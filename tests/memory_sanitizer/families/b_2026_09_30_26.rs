//! B-2026-09-30-26 -- whole by-value params moved into a returned `Vec`
//! literal run each `Drop` body once.

use super::*;

/// B-2026-09-30-26 -- the `Vec` twin of B-2026-09-30-22: a whole by-value
/// param moved into a `Vec` literal the callee returns (`return vec![a, b]`,
/// or a `Vec`-typed `return [a, b]`, both of which reach the ownership
/// predicates as a prefix collection literal). Those predicates had no arm
/// for that literal, so the caller's argument ran its body beside the
/// vector's owner on every surface. Covers temporary and named arguments, a
/// block-tail literal, the `[a, b]` spelling, a conditional `return` and a
/// conditional tail on both paths, a fresh element beside the param, a
/// method, and a param's FIELDS in the literal (the part channel).
#[test]
fn asan_whole_params_in_returned_vec_literal_run_each_body_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
struct W3 { q: R, r: R, n: i64 }
struct K { a: i64 }
impl K { fn vm(ref self, a: R, b: R) -> Vec[R] { return vec![a, b]; } }
fn vc(a: R, b: R) -> Vec[R] { return vec![a, b]; }
fn vt(a: R, b: R) -> Vec[R] { vec![a, b] }
fn va(a: R, b: R) -> Vec[R] { return [a, b]; }
fn vcc(a: R, f: bool) -> Vec[R] { if f { return vec![a]; } return vec![mk(90)]; }
fn vtc(a: R, f: bool) -> Vec[R] { if f { vec![a] } else { vec![mk(91)] } }
fn vmix(a: R) -> Vec[R] { return vec![a, mk(50)]; }
fn vf(w: W3) -> Vec[R] { return vec![w.q, w.r]; }
fn main() {
  println("-c1"); let c1 = vc(mk(1), mk(2)); println(f"r{c1.len()}{c1[0].id}")
  println("-c2"); let p = mk(3); let q = mk(4); let c2 = vc(p, q); println(f"r{c2[1].id}")
  println("-c4"); let c4 = vt(mk(7), mk(8)); println(f"r{c4[1].id}")
  println("-c5"); let c5 = va(mk(9), mk(10)); println(f"r{c5[0].id}")
  println("-c6"); let c6 = vcc(mk(11), true); println(f"r{c6[0].id}")
  println("-c7"); let c7 = vcc(mk(12), false); println(f"r{c7[0].id}")
  println("-c8"); let c8 = vtc(mk(13), true); println(f"r{c8[0].id}")
  println("-c9"); let c9 = vtc(mk(14), false); println(f"r{c9[0].id}")
  println("-c10"); let c10 = vmix(mk(15)); println(f"r{c10[0].id}{c10[1].id}")
  println("-c11"); let k = K { a: 1 }; let c11 = k.vm(mk(16), mk(17)); println(f"r{c11[1].id}")
  println("-c12"); let c12 = vf(W3 { q: mk(18), r: mk(19), n: 1 }); println(f"r{c12[1].id}")
  println("-c13"); let w = W3 { q: mk(20), r: mk(21), n: 2 }; let c13 = vf(w); println(f"r{c13[0].id}")
  println("end")
}
"#,
        &[
            "-c1", "r21", "dR1", "dR2", "-c2", "r4", "dR3", "dR4", "-c4", "r8", "dR7", "dR8",
            "-c5", "r9", "dR9", "dR10", "-c6", "r11", "dR11", "-c7", "dR12", "r90", "dR90", "-c8",
            "r13", "dR13", "-c9", "dR14", "r91", "dR91", "-c10", "r1550", "dR15", "dR50", "-c11",
            "r17", "dR16", "dR17", "-c12", "r19", "dR18", "dR19", "-c13", "r20", "dR20", "dR21",
            "end",
        ],
        "whole_params_in_returned_vec_literal_run_each_body_once",
    );
}

/// B-2026-09-30-22 / -26 -- the kata thread's independent grid over the same
/// shapes (its withdrawn duplicate of -22): `Array` and `Vec` literals, both
/// the bare and the `Vec[..]` / `Array[..]` prefix spellings, a conditional
/// return and tail, a fresh element beside the param, an array inside a
/// tuple, a forwarding callee, a method, a temporary array handed straight
/// on, and the tuple twin as a guard.
#[test]
fn asan_array_and_vec_literal_hand_backs_run_each_body_once() {
    assert_clean_asan_run(
        r#"struct D { id: i64, s: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}") } }
fn mkd(n: i64) -> D { return D { id: n, s: f"sssssssssssssssssssssssssssssssssss{n}" } }
struct K { n: i64 }
impl K { fn vpair(ref self, a: D, b: D) -> Vec[D] { Vec[a, b] } }
fn tail(a: D, b: D) -> Array[D, 2] { [a, b] }
fn usea(xs: Array[D, 2]) -> i64 { return xs[0].id + xs[1].id }
fn vtail(a: D, b: D) -> Vec[D] { Vec[a, b] }
fn keep1(a: D, b: D) -> Array[D, 1] { [a] }
fn condv(a: D, k: bool) -> Vec[D] { if k { return Vec[a]; } Vec[mkd(90)] }
fn witha(a: D) -> Array[D, 2] { return Array[a, mkd(91)]; }
fn nest(a: D, b: D) -> (Array[D, 1], D) { ([a], b) }
fn fwd(a: D) -> Array[D, 1] { keep1(a, mkd(92)) }
fn condt(a: D, k: bool) -> Array[D, 1] { if k { [a] } else { [mkd(93)] } }
fn tup(a: D, b: D) -> (D, D) { (a, b) }
fn main() {
    println(f"a {usea(tail(mkd(1), mkd(2)))}")
    { let x = tail(mkd(3), mkd(4)); println(f"b {x[0].id}") }
    { let p = mkd(5); let q = mkd(6); let x = tail(p, q); println(f"c {x[1].id}") }
    { let x = vtail(mkd(7), mkd(8)); println(f"d {x.len()}") }
    { let x = keep1(mkd(9), mkd(10)); println(f"e {x[0].id}") }
    { let p = mkd(11); let q = mkd(12); let x = keep1(p, q); println(f"f {x[0].id}") }
    { let x = condv(mkd(13), true); println(f"g {x.len()}") }
    { let x = condv(mkd(14), false); println(f"h {x[0].id}") }
    { let x = witha(mkd(15)); println(f"i {x[1].id}") }
    { let k = K { n: 0 }; let x = k.vpair(mkd(16), mkd(17)); println(f"j {x.len()}") }
    { let x = nest(mkd(18), mkd(19)); println(f"k {x.1.id}") }
    { let x = fwd(mkd(20)); println(f"l {x[0].id}") }
    { let p = mkd(21); let x = condt(p, true); println(f"m {x[0].id}") }
    { let p = mkd(22); let x = condt(p, false); println(f"n {x[0].id}") }
    { let x = tup(mkd(23), mkd(24)); println(f"o {x.0.id}") }
    println("end")
}
"#,
        &[
            "dD1", "dD2", "a 3", "b 3", "dD3", "dD4", "c 6", "dD5", "dD6", "d 2", "dD7", "dD8",
            "dD10", "e 9", "dD9", "dD12", "f 11", "dD11", "g 1", "dD13", "dD14", "h 90", "dD90",
            "i 91", "dD15", "dD91", "j 2", "dD16", "dD17", "k 19", "dD18", "dD19", "dD92", "l 20",
            "dD20", "m 21", "dD21", "dD22", "n 93", "dD93", "o 23", "dD23", "dD24", "end",
        ],
        "array_and_vec_literal_hand_backs_run_each_body_once",
    );
}
