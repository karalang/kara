//! B-2026-10-04-73: reassigning a tuple element that has moved out runs the new value's Drop body in the interpreter

use super::*;

/// The interpreter half of B-2026-10-04-73: the `(t, 0)` move-out record
/// outlived the store, so the replacement's body ran nowhere. Same program as
/// the memory-sanitizer fixture, so the two backends are pinned to one answer.
#[test]
fn interp_reassigned_moved_out_tuple_elem_runs_new_body() {
    let out = run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct P { r: R, k: i64 }
enum E { A(R), B }
fn mk(i: i64) -> R { R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn plain() { let mut t: (R, i64) = (mk(1), 0); let x = t.0; t.0 = mk(2); println(f"x{x.id}"); println(f"a{t.1}") }
fn movedthen(c: bool) { let mut t: (R, i64) = (mk(3), 0); if c { let x = t.0; println(f"x{x.id}"); } t.0 = mk(4); println(f"b{t.1}") }
fn samepath(c: bool) { let mut t: (R, i64) = (mk(5), 0); if c { let x = t.0; println(f"x{x.id}"); t.0 = mk(6); } println(f"c{t.1}") }
fn deeper(c: bool) { let mut t: (R, i64) = (mk(7), 0); let x = t.0; println(f"x{x.id}"); if c { t.0 = mk(8); } println(f"d{t.1}") }
fn inloop() { let mut t: (R, i64) = (mk(9), 0); let x = t.0; println(f"x{x.id}"); for i in 10..13 { t.0 = mk(i); } println(f"e{t.1}") }
fn twice() { let mut t: (R, R) = (mk(14), mk(15)); let x = t.0; t.0 = mk(16); t.0 = mk(17); println(f"x{x.id} f{t.1.id}") }
fn nested() { let mut t: (P, i64) = (P { r: mk(18), k: 1 }, 0); let x = t.0; println(f"x{x.k}"); t.0 = P { r: mk(19), k: 2 }; println(f"g{t.1}") }
fn opt() { let mut t: (Option[R], i64) = (Some(mk(20)), 0); let x = t.0; t.0 = Some(mk(21)); println(f"h{t.1}") }
fn en() { let mut t: (E, i64) = (E.A(mk(22)), 0); let x = t.0; t.0 = E.A(mk(23)); println(f"i{t.1}") }
fn main() {
    plain();
    movedthen(true); movedthen(false);
    samepath(true); samepath(false);
    deeper(true); deeper(false);
    inloop(); twice(); nested(); opt(); en();
    println("end");
}
"#);
    assert_eq!(out, "x1\ndR1\na0\ndR2\nx3\ndR3\nb0\ndR4\ndR3\nb0\ndR4\nx5\ndR5\nc0\ndR6\nc0\ndR5\nx7\ndR7\nd0\ndR8\nx7\ndR7\nd0\nx9\ndR9\ndR10\ndR11\ne0\ndR12\ndR16\nx14 f15\ndR14\ndR17\ndR15\nx1\ndR18\ng0\ndR19\ndR20\nh0\ndR21\ndR22\ni0\ndR23\nend\n");
}
