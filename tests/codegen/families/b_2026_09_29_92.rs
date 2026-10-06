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
fn e2e_fresh_struct_handed_on_by_value_releases_its_payloads_once() {
    let Some(out) = run_program(
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
    ) else {
        return;
    };
    assert_eq!(out, "g1\n_1\ng2\n_2\nm3\n_3\nm4\n_4\ng5\n_5\ng6\n_6\nk7\ndH7\n_7\nc8\ndH8\n_8\nm9\n_9\nx10\ndH10\n_10\nk11\ndH11\n_11\nend\n", "got:\n{out}");
}
