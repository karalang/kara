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
fn e2e_fresh_temp_field_returned_by_method_runs_siblings_once() {
    let src = r#"struct D { id: i64, name: String }
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
}"#;
    let want = "d101n101\na1\nd1n1\nd102n102\nb2\nd2n2\nd303n303\nd103n103\nc3\nd3n3\nd4n4\ne104\nd104n104\nd105n105\nd5n5\nf5\nd106n106\ng6\nd6n6\nd307n307\ni7\nd107n107\nd7n7\nd108n108\nj8\nd8n8\nd109n109\nk9\nd9n9\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
