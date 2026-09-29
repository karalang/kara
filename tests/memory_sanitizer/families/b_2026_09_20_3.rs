//! B-2026-09-20-3 -- a CHAINED place's boxed enum field handed to a by-value
//! callee (`eatb(k.h.g)`) is freed once, by the callee.

use super::*;

/// B-2026-09-20-3 — `eatb(k.h.g)` and `eatb(m.k.h.g)` over a non-generic
/// `enum Eb { A(Array[String, 2]), B }`, including inside a loop, beside
/// the controls that were already clean: a `ref` callee, a `Drop`-bodied
/// `Array` element, `String` and `Vec` payloads, a generic `G1[String]`
/// field and a unit variant.
///
/// Before: the two- and three-hop cells died with a segfault at `-O0` and
/// `free(): double free detected` at `-O2` (7 valgrind errors).
#[test]
fn asan_chained_boxed_enum_field_handed_to_callee_frees_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  d{self.id}") } }
enum Eb { A(Array[String, 2]), B }
enum Er { A(Array[R, 2]), B }
enum Es { A(String), B }
enum Ev { A(Vec[String]), B }
enum G1[T] { Y(T), N }
struct Hb { g: Eb }
struct Kb { h: Hb }
struct Mb { k: Kb }
struct Hr { g: Er }
struct Kr { h: Hr }
struct Hs { g: Es }
struct Ks { h: Hs }
struct Hv { g: Ev }
struct Kv { h: Hv }
struct Hg { g: G1[String] }
struct Kg { h: Hg }
shared struct Sh { g: Eb }
struct Ksh { h: Sh }
fn eatb(g: Eb) -> i64 { match g { Eb.A(a) => { return a[0].len() }, Eb.B => { return 0 } } }
fn peekb(g: ref Eb) -> i64 { match g { Eb.A(a) => { return a[1].len() }, Eb.B => { return 0 } } }
fn eatr(g: Er) -> i64 { match g { Er.A(a) => { return a[0].id }, Er.B => { return 0 } } }
fn eats(g: Es) -> i64 { match g { Es.A(s) => { return s.len() }, Es.B => { return 0 } } }
fn eatv(g: Ev) -> i64 { match g { Ev.A(v) => { return v.len() }, Ev.B => { return 0 } } }
fn eatg(g: G1[String]) -> i64 { match g { G1.Y(s) => { return s.len() }, G1.N => { return 0 } } }
fn mk() -> Array[String, 2] { return [f"aaaaaaaa-1", f"bbbbbbbb-22"]; }
fn mkr(i: i64) -> R { return R { id: i, s: f"r{i}" } }
fn main() {
    let m = Mb { k: Kb { h: Hb { g: Eb.A(mk()) } } };
    println(f"three {eatb(m.k.h.g)}");
    let k = Kb { h: Hb { g: Eb.A(mk()) } };
    println(f"peek {peekb(k.h.g)}");
    println(f"after {eatb(k.h.g)}");
    let kr = Kr { h: Hr { g: Er.A([mkr(1), mkr(2)]) } };
    println(f"drops {eatr(kr.h.g)}");
    let ks = Ks { h: Hs { g: Es.A(f"strpay") } };
    println(f"str {eats(ks.h.g)}");
    let kv = Kv { h: Hv { g: Ev.A(vec![f"x1", f"x2", f"x3"]) } };
    println(f"vec {eatv(kv.h.g)}");
    let kg = Kg { h: Hg { g: G1.Y(f"generic") } };
    println(f"gen {eatg(kg.h.g)}");
    let mut i = 0;
    while i < 2 { let kl = Kb { h: Hb { g: Eb.A(mk()) } }; println(f"loop {eatb(kl.h.g)}"); i = i + 1; }
    let kb2 = Kb { h: Hb { g: Eb.B } };
    println(f"unit {eatb(kb2.h.g)}");
    println("end")
}"#,
        &[
            "three 10", "peek 11", "after 10", "drops 1", "  d1", "  d2", "str 6", "vec 3",
            "gen 7", "loop 10", "loop 10", "unit 0", "end",
        ],
        "chained_boxed_enum_field_to_callee",
    );
}
