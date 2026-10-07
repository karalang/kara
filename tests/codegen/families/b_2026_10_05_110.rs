//! B-2026-10-05-110, -111 — reassigning a moved-out tuple, Option or Vec local re-arms its bodies.

use super::*;

/// B-2026-10-05-110, -111 — a tuple, `Option` or `Vec` local that was moved
/// out whole (or had a tuple element moved out) and is then reassigned runs
/// the NEW value's `Drop` bodies when it dies, at its own position in reverse
/// declaration order (`a`, `d`, `h`, `i`, `l`), and a displaced value runs its
/// bodies at the store (`d`, `j`). Conditional moves and conditional stores
/// (`f`, `g`, `k`) keep the same answers.
///
/// Before: the move retracted the binding's walk and the store never put it
/// back, so the new value's bodies ran on no compiled surface; a moved `Vec`
/// instead ran the MOVED-OUT value's bodies twice, once at the store (`l`, `m`).
#[test]
fn e2e_reassigned_moved_local_rearms_bodies() {
    let src = r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> (R, i64) { (R { id: i }, i) }
fn a() { let mut p = mkr(1); let q = p; p = mkr(2); println(f"a{p.1}{q.1}") }
fn b() { let mut p = mkr(3); let x = p.0; p = mkr(4); println(f"b{p.1}{x.id}") }
fn c() { let mut p = mkr(5); let (x, y) = p; p = mkr(6); println(f"c{p.1}{x.id}{y}") }
fn d() { let mut p = mkr(7); let q = p; p = mkr(8); p = mkr(9); println(f"d{p.1}{q.1}") }
fn e() { let mut p = (R { id: 10 }, R { id: 11 }); let x = p.0; p = (R { id: 12 }, R { id: 13 }); println(f"e{x.id}") }
fn f() { let mut p = mkr(14); let q = p; if q.1 > 0 { p = mkr(15); } println(f"f{q.1}") }
fn g() { let mut p = mkr(16); if p.1 > 0 { let q = p; println(f"g{q.1}"); } p = mkr(17); println(f"g{p.1}") }
fn h() { let mut p = mkr(18); let x = p.0; p = mkr(19); let y = p.0; p = mkr(20); println(f"h{p.1}{x.id}{y.id}") }
fn i() { let mut o = Some(R { id: 21 }); let q = o; o = Some(R { id: 22 }); println(f"i{o.is_some()}{q.is_some()}") }
fn j() { let mut o = Some(R { id: 23 }); let q = o; o = None; o = Some(R { id: 24 }); println(f"j{q.is_some()}") }
fn k() { let mut o = Some(R { id: 25 }); if o.is_some() { let q = o; println(f"k{q.is_some()}"); } o = Some(R { id: 26 }); println(f"k{o.is_some()}") }
fn l() { let mut v = [R { id: 27 }]; let q = v; v = [R { id: 28 }]; println(f"l{v.len()}{q.len()}") }
fn m() { let mut v: Vec[R] = [R { id: 29 }]; let q = v; v = [R { id: 30 }, R { id: 31 }]; println(f"m{v.len()}{q.len()}") }
fn n() { let mut v = [R { id: 32 }]; v = [R { id: 33 }]; println(f"n{v.len()}") }
fn main() {
    a();
    b();
    c();
    d();
    e();
    f();
    g();
    h();
    i();
    j();
    k();
    l();
    m();
    n();
    println("end")
}
"#;
    let want = "a21\ndR1\ndR2\nb43\ndR3\ndR4\nc655\ndR5\ndR6\ndR8\nd97\ndR7\ndR9\ndR11\ndR12\ndR13\ne10\ndR10\ndR15\nf14\ndR14\ng16\ndR16\ng17\ndR17\nh201819\ndR19\ndR18\ndR20\nitruetrue\ndR21\ndR22\ndR24\njtrue\ndR23\nktrue\ndR25\nktrue\ndR26\nl11\ndR27\ndR28\nm21\ndR29\ndR30\ndR31\ndR32\nn1\ndR33\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
