//! B-2026-10-04-65 — a destructured or nested `shared` tuple element is released.

use super::*;

/// B-2026-10-04-65 — a `shared` element of a fresh tuple, bare or in an
/// `Option`, is released by whoever takes it: a destructure leaf (bound, a
/// wildcard, nested, off a generic callee's `(T, i64)`), the leaf of a named
/// tuple's `Option[shared]` element (`b`, `o`), and a tuple literal that a
/// named tuple is moved into (`c`, `e`, `p`, `w`). A leaf moved on (`i`-`l`,
/// `u`, `v`) is released by its new owner.
///
/// Before: compiled code leaked each box (16 B) and ran no `Drop` body, and
/// the interpreter ran none for a wildcard (`g`, `h`, `n`, `r`).
#[test]
fn asan_tuple_destructure_releases_shared_leaf() {
    assert_clean_asan_run_min_allocs(
        r#"shared struct H { id: i64 }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct S { t: (Option[H], i64) }
fn mk(i: i64) -> (Option[H], i64) { (Some(H { id: i }), i) }
fn mks(i: i64) -> (H, i64) { (H { id: i }, i) }
fn mkh(i: i64) -> H { H { id: i } }
fn take(h: H) -> i64 { h.id }
fn takeo(o: Option[H]) -> i64 { match o { Some(h) => h.id, None => 0 } }
fn ret(i: i64) -> H { let (a, b) = mks(i); a }
fn gn[T](x: T) -> (T, i64) { (x, 1) }
fn main() {
    { let (a1, b1) = mk(1); println(f"a{a1.unwrap().id} {b1}") }
    { let p2 = mk(2); let (a2, b2) = p2; println(f"b{b2}") }
    { let p3 = mk(3); let r3 = ((p3, 5), 6); println(f"c{r3.1}") }
    { let (a4, b4) = mks(4); println(f"d{a4.id} {b4}") }
    { let p5 = mks(5); let r5 = ((p5, 5), 6); println(f"e{r5.1}") }
    { let (a6, b6) = mks(6); println(f"f{b6}") }
    { let (_, b7) = mks(7); println(f"g{b7}") }
    { let (_, b8) = mk(8); println(f"h{b8}") }
    { let (a9, b9) = mks(9); let q9 = a9; println(f"i{q9.id}") }
    { let (a10, b10) = mks(10); println(f"j{take(a10)}") }
    { let (a11, b11) = mk(11); println(f"k{takeo(a11)}") }
    { let h12 = ret(12); println(f"l{h12.id}") }
    { let ((a13, b13), c13) = (mks(13), 2); println(f"m{a13.id}") }
    { let (_, b14) = (mkh(14), 1); println(f"n{b14}") }
    { let s15 = S { t: mk(15) }; let (a15, b15) = s15.t; println(f"o{b15}") }
    { let p16 = (mkh(16), R { id: 16 }); let r16 = (p16, 6); println(f"p{r16.1}") }
    { let (a17, b17) = gn(mkh(17)); println(f"q{a17.id}") }
    { let (_, b18) = gn(mkh(18)); println(f"r{b18}") }
    { let (a19, b19) = gn(Some(mkh(19))); println(f"s{b19}") }
    { for i in 20..22 { let (a, b) = mks(i); println(f"t{a.id}") } }
    { let (a22, b22) = mks(22); let v22 = [a22]; println(f"u{v22.len()}") }
    { let x23 = { let (a23, b23) = mks(23); a23 }; println(f"v{x23.id}") }
    { let p24 = mks(24); let r24 = (p24, 6); let (q24, n24) = r24; println(f"w{n24}") }
    println("end")
}"#,
        &[
            "a1 1", "dH1", "b2", "dH2", "c6", "dH3", "d4 4", "dH4", "e6", "dH5", "dH6", "f6",
            "dH7", "g7", "dH8", "h8", "i9", "dH9", "j10", "dH10", "k11", "dH11", "l12", "dH12",
            "m13", "dH13", "dH14", "n1", "o15", "dH15", "p6", "dR16", "dH16", "q17", "dH17",
            "dH18", "r1", "s1", "dH19", "t20", "dH20", "t21", "dH21", "u1", "dH22", "v23", "dH23",
            "w6", "dH24", "end",
        ],
        "asan_tuple_destructure_releases_shared_leaf",
        8,
    );
}
