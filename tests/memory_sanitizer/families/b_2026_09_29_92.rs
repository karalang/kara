//! B-2026-09-29-92: a fresh struct handed on by value releases its shared payloads once

use super::*;

/// B-2026-09-29-92 — a copy-supported struct handed on by value is ENTRY-COPIED,
/// and the copy leaves its boxed `Array` and `shared` enum payloads shared with
/// the caller, whose drop then released them a second time: a temp argument
/// (`_1`, `_2`, `_5`, the spelling B-2026-10-05-118 fixed), a temp RECEIVER of an
/// owned-`self` method (`_3`, `_4`, `_7`, `_11`) and a struct FIELD handed on
/// whole (`_6`, `_8`). The interpreter released nothing a fresh owned-`self`
/// receiver held (`_7`, `_10`, `_11`); every surface now prints it after the call.
#[test]
fn asan_fresh_struct_handed_on_by_value_releases_its_payloads_once() {
    assert_clean_asan_run(
        r#"shared struct H { id: i64 }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
enum E { A(H), B }
struct K { e: E, n: i64 }
impl K { fn take(self) { println(f"k{self.n}") } }
struct X { h: H, n: i64 }
impl X { fn take(self) { println(f"x{self.n}") } }
fn mkk(i: i64) -> K { K { e: E.A(H { id: i }), n: i } }
enum Eb { A(Array[String, 2]), B }
struct Hb { g: Eb, n: i64 }
struct Wr { h: Hb }
struct Wk { k: K }
fn give(h: Hb) { println(f"g{h.n}") }
fn ck(k: K) { println(f"c{k.n}") }
fn outer(h: Hb) { give(h) }
fn mk(i: i64) -> Array[String, 2] { return [f"aaaaaaaa-{i}", f"bbbbbbbb-{i}"]; }
fn mkh(i: i64) -> Hb { return Hb { g: Eb.A(mk(i)), n: i } }
impl Hb { fn mine(self) { println(f"m{self.n}") } }
fn main() {
    give(Hb { g: Eb.A(mk(1)), n: 1 }); println("_1");
    give(mkh(2)); println("_2");
    Hb { g: Eb.A(mk(3)), n: 3 }.mine(); println("_3");
    mkh(4).mine(); println("_4");
    outer(Hb { g: Eb.A(mk(5)), n: 5 }); println("_5");
    let w = Wr { h: Hb { g: Eb.A(mk(6)), n: 6 } }; give(w.h); println("_6");
    K { e: E.A(H { id: 7 }), n: 7 }.take(); println("_7");
    let wk = Wk { k: K { e: E.A(H { id: 8 }), n: 8 } }; ck(wk.k); println("_8");
    let h9 = Hb { g: Eb.A(mk(9)), n: 9 }; h9.mine(); println("_9");
    X { h: H { id: 10 }, n: 10 }.take(); println("_10");
    mkk(11).take(); println("_11");
    println("end")
}
"#,
        &[
            "g1", "_1", "g2", "_2", "m3", "_3", "m4", "_4", "g5", "_5", "g6", "_6", "k7", "dH7",
            "_7", "c8", "dH8", "_8", "m9", "_9", "x10", "dH10", "_10", "k11", "dH11", "_11", "end",
        ],
        "asan_fresh_struct_handed_on_by_value_releases_its_payloads_once",
    );
}
