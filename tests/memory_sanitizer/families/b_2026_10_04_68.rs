//! B-2026-10-04-68 — storing into a tuple element or field drops what it displaced.

use super::*;

/// B-2026-10-04-68 — storing a new value into a tuple element (`p.0 = ...`)
/// runs the `Drop` body of the `shared` value it displaces, bare or in an
/// `Option` (`a`-`i`). A projection assigned to itself (`p.0 = p.0`,
/// `w.h = w.h`, B-2026-10-06-4) is a no-op (`j`-`n`).
///
/// Before: the interpreter ran no `Drop` body for a displaced `shared`
/// element, and compiled code released the element and then stored the freed
/// pointer back on a self-assignment, a use-after-free.
#[test]
fn asan_tuple_elem_store_drops_displaced_shared() {
    assert_clean_asan_run_min_allocs(
        r#"shared struct H { id: i64 }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
struct W { h: H, n: i64 }
fn mk(i: i64) -> (Option[H], i64) { (Some(H { id: i }), i) }
fn mks(i: i64) -> (H, i64) { (H { id: i }, i) }
fn mkh(i: i64) -> H { H { id: i } }
fn mkst(i: i64) -> (String, i64) { (f"s{i}xxxxxxxxxxxxxxxxxxxxxxxxxxxxxx", i) }
fn a() { let mut p = mk(1); p.0 = Some(H { id: 2 }); println(f"a{p.1}") }
fn b() { let mut p = mks(3); p.0 = H { id: 4 }; println(f"b{p.1}") }
fn c() { let mut p = mk(5); p.0 = None; println(f"c{p.1}") }
fn d() { let mut p = (mkh(6), 7); p.0 = mkh(8); println(f"d{p.1}") }
fn e() { let mut p = mk(9); let x = p.0; p.0 = Some(H { id: 10 }); println(f"e{p.1} {x.is_some()}") }
fn f() { let mut p = mk(11); p.0 = Some(H { id: 12 }); p.0 = None; println(f"f{p.1}") }
fn g() { let h = H { id: 13 }; let mut p = (Some(h), 14); p.0 = None; println(f"g{p.1}") }
fn i() { let h = H { id: 15 }; let mut p = (h, 16); p.0 = H { id: 17 }; println(f"i{p.1}") }
fn j() { let mut p = mk(18); p.0 = p.0; println(f"j{p.1}") }
fn k() { let mut p = mks(19); p.0 = p.0; println(f"k{p.1} {p.0.id}") }
fn l() { let mut p = mk(20); p.0 = p.0; println(f"l{p.1} {p.0.unwrap().id}") }
fn m() { let mut p = mkst(21); p.0 = p.0; println(f"m{p.1} {p.0}") }
fn n() { let mut w = W { h: H { id: 22 }, n: 23 }; w.h = w.h; println(f"n{w.h.id} {w.n}") }
fn main() {
    a();
    b();
    c();
    d();
    e();
    f();
    g();
    i();
    j();
    k();
    l();
    m();
    n();
    println("end")
}"#,
        &[
            "dH1",
            "a1",
            "dH2",
            "dH3",
            "b3",
            "dH4",
            "dH5",
            "c5",
            "dH6",
            "d7",
            "dH8",
            "e9 true",
            "dH9",
            "dH10",
            "dH11",
            "dH12",
            "f11",
            "dH13",
            "g14",
            "dH15",
            "i16",
            "dH17",
            "j18",
            "dH18",
            "k19 19",
            "dH19",
            "l20 20",
            "dH20",
            "m21 s21xxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
            "n22 23",
            "dH22",
            "end",
        ],
        "asan_tuple_elem_store_drops_displaced_shared",
        8,
    );
}
