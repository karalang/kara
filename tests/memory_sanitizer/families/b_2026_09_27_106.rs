//! B-2026-09-27-106 — a method that returns one field of a FRESH-TEMP struct
//! (a call-result argument or a call-result receiver) runs every other
//! field's `Drop` body once, at the statement.

use super::*;

/// B-2026-09-27-106 — `h.g(mkw(1))` over `fn g(ref self, w: W) -> D { w.r }`
/// lost the sibling's body under `--interp`; `mkw(2).getr()` over
/// `fn getr(self) -> D { self.r }` lost it interpreted and ran it after
/// `main`'s last line compiled; two hops down (`mkt(3).deep()` over
/// `self.w.r`) compiled code lost both leftovers. The named spellings (`j`,
/// `k`) were right everywhere, and `f` / `g` / `i` pin the neighbours a
/// second walk on the receiver temp would double.
#[test]
fn asan_fresh_temp_field_returned_by_method_is_freed_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"d{self.id}{self.name}") } }
fn mkd(n: i64) -> D { D { id: n, name: f"n{n}" } }
struct W { r: D, s: D, b: i64 }
fn mkw(n: i64) -> W { W { r: mkd(n), s: mkd(n + 100), b: n } }
struct T { w: W, t: D }
fn mkt(n: i64) -> T { T { w: mkw(n), t: mkd(n + 300) } }
struct H { k: i64 }
impl H { fn g(ref self, w: W) -> D { w.r } }
impl W {
    fn getr(self) -> D { self.r }
    fn gets(self) -> D { return self.s; }
    fn getb(self) -> i64 { self.b }
    fn dest(self) -> D { let W { r, s, b } = self; r }
}
impl T {
    fn deep(self) -> D { self.w.r }
    fn getw(self) -> W { self.w }
}
fn main() {
    let h = H { k: 0 };
    let a = h.g(mkw(1));
    println(f"a{a.id}");
    let b = mkw(2).getr();
    println(f"b{b.id}");
    let c = mkt(3).deep();
    println(f"c{c.id}");
    let e = mkw(4).gets();
    println(f"e{e.id}");
    println(f"f{mkw(5).getb()}");
    let g = mkw(6).dest();
    println(f"g{g.id}");
    let w = mkt(7).getw();
    println(f"i{w.b}");
    let v = mkw(8);
    let j = v.getr();
    println(f"j{j.id}");
    let x = mkw(9);
    let k = h.g(x);
    println(f"k{k.id}");
    println("end")
}
"#,
        &[
            "d101n101", "a1", "d1n1", "d102n102", "b2", "d2n2", "d303n303", "d103n103", "c3",
            "d3n3", "d4n4", "e104", "d104n104", "d105n105", "d5n5", "f5", "d106n106", "g6", "d6n6",
            "d307n307", "i7", "d107n107", "d7n7", "d108n108", "j8", "d8n8", "d109n109", "k9",
            "d9n9", "end",
        ],
        "asan_fresh_temp_field_returned_by_method_is_freed_once",
        30,
    );
}
