//! B-2026-10-06-36 — a nested tuple element moved out of a tuple local keeps its bodies.

use super::*;

/// B-2026-10-06-36 — `let q = p.0` over a tuple local whose element is
/// itself a tuple hands `q` the element's `Drop` bodies, and `q`'s run before
/// `p`'s remaining ones when both die at once (`d`, `f`). A `shared` member
/// is released (`e`) and a heap field freed once (`c`, `i`).
///
/// Before: `p`'s walk masked the element and `q` registered none, so its
/// bodies ran on no compiled surface (and `e` leaked the box); where `p`
/// still had bodies, its walk was re-pushed after `q`'s and ran first.
#[test]
fn asan_moved_nested_tuple_elem_runs_its_bodies() {
    assert_clean_asan_run_min_allocs(
        r#"shared struct H { id: i64 }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct W { r: R, s: String }
fn mkr(i: i64) -> (R, i64) { (R { id: i }, i) }
fn mk(i: i64) -> (Option[H], i64) { (Some(H { id: i }), i) }
fn mkw(i: i64) -> (W, i64) { (W { r: R { id: i }, s: f"s{i}xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx" }, i) }
fn a() { let p = (mkr(1), 5); let q = p.0; println(f"a{p.1} {q.1}") }
fn b() { let p = ((R { id: 2 }, 3), 5); let q = p.0; println(f"b{p.1} {q.1}") }
fn c() { let p = (mkw(4), 5); let q = p.0; println(f"c{p.1} {q.0.s}") }
fn d() { let p = (mkr(6), mkr(7)); let q = p.0; println(f"d{p.1.1} {q.1}") }
fn e() { let p = (mk(8), 5); let q = p.0; println(f"e{p.1} {q.1}") }
fn f() { let mut p = (mkr(9), 5); let q = p.0; p.0 = mkr(10); println(f"f{p.1} {q.1}") }
fn g() { let p = (mkr(11), 5); if p.1 > 3 { let q = p.0; println(f"g{q.1}") } println(f"g{p.1}") }
fn h() { let p = (mkr(12), 5); let q = p.0; let r = q; println(f"h{p.1} {r.1}") }
fn i() { let p = (mkw(13), 5); if p.1 > 3 { let q = p.0; println(f"i{q.1}") } println(f"i{p.1}") }
fn j() { let p = (mkr(14), mkr(15)); let q = p.0; println(f"j{q.1}"); println(f"j{p.1.1}") }
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
    println("end")
}"#,
        &[
            "a5 1",
            "dR1",
            "b5 3",
            "dR2",
            "c5 s4xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
            "dR4",
            "d7 6",
            "dR6",
            "dR7",
            "e5 8",
            "dH8",
            "f5 9",
            "dR9",
            "dR10",
            "g11",
            "dR11",
            "g5",
            "h5 12",
            "dR12",
            "i13",
            "dR13",
            "i5",
            "j14",
            "dR14",
            "j15",
            "dR15",
            "end",
        ],
        "asan_moved_nested_tuple_elem_runs_its_bodies",
        8,
    );
}
