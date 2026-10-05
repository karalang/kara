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
fn e2e_tuple_destructure_releases_shared_leaf() {
    let src = r#"shared struct H { id: i64 }
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
}
"#;
    let want = "a1 1\ndH1\nb2\ndH2\nc6\ndH3\nd4 4\ndH4\ne6\ndH5\ndH6\nf6\ndH7\ng7\ndH8\nh8\ni9\ndH9\nj10\ndH10\nk11\ndH11\nl12\ndH12\nm13\ndH13\ndH14\nn1\no15\ndH15\np6\ndR16\ndH16\nq17\ndH17\ndH18\nr1\ns1\ndH19\nt20\ndH20\nt21\ndH21\nu1\ndH22\nv23\ndH23\nw6\ndH24\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
