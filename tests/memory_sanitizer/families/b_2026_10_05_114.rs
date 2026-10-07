//! B-2026-10-05-114 — storing a nested tuple element drops what it displaced.

use super::*;

/// B-2026-10-05-114 — storing into a tuple element that is itself a tuple
/// (`p.0 = mkr(2)` over `((R, i64), i64)`) runs the displaced inner tuple's
/// element bodies at the store, and releases a `shared` value it held (`c`).
/// A heap field in the displaced element is freed once (`d`).
///
/// Before: no surface ran a displaced nested tuple's bodies, and the
/// interpreter also never released its `shared` member.
#[test]
fn asan_nested_tuple_elem_store_drops_displaced() {
    assert_clean_asan_run_min_allocs(
        r#"shared struct H { id: i64 }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct W { r: R, s: String }
fn mkr(i: i64) -> (R, i64) { (R { id: i }, i) }
fn mk(i: i64) -> (Option[H], i64) { (Some(H { id: i }), i) }
fn mkw(i: i64) -> (W, i64) { (W { r: R { id: i }, s: f"s{i}xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx" }, i) }
fn a() { let mut p = (mkr(1), 5); p.0 = mkr(2); p = (mkr(3), 6); println(f"a{p.1}") }
fn b() { let mut p = (mkr(4), 5); p.0 = mkr(5); println(f"b{p.1} {p.0.1}") }
fn c() { let mut p = (mk(6), 5); p.0 = mk(7); println(f"c{p.1} {p.0.1}") }
fn d() { let mut p = (mkw(8), 5); p.0 = mkw(9); println(f"d{p.1} {p.0.0.s}") }
fn e() { let mut p = ((R { id: 10 }, R { id: 11 }), 5); p.0 = (R { id: 12 }, R { id: 13 }); println(f"e{p.1}") }
fn f() { let mut p = (mkr(14), mkr(15)); p.1 = mkr(16); println(f"f{p.0.1} {p.1.1}") }
fn main() {
    a();
    b();
    c();
    d();
    e();
    f();
    println("end")
}"#,
        &[
            "dR1",
            "dR2",
            "a6",
            "dR3",
            "dR4",
            "b5 5",
            "dR5",
            "dH6",
            "c5 7",
            "dH7",
            "dR8",
            "d5 s9xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
            "dR9",
            "dR10",
            "dR11",
            "e5",
            "dR12",
            "dR13",
            "dR15",
            "f14 16",
            "dR14",
            "dR16",
            "end",
        ],
        "asan_nested_tuple_elem_store_drops_displaced",
        8,
    );
}
