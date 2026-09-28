//! B-2026-09-28-20 — the remainder of B-2026-09-27-105: a `Drop` part handed
//! to another owner on only some paths, off a METHOD's by-value param or an
//! owned receiver, runs its body once.

use super::*;

/// B-2026-09-28-20 cells (g) and (h) — `fn ret(ref self, w: W, c: bool)` over
/// `if c { keep(w.r) }`, and `fn ret(self, c: bool)` over `if c {
/// keep(self.r) }`, ran `w.r`'s body on the keeping leg in the callee's new
/// owner AND in the caller's walk, on all four surfaces: the per-path adoption
/// B-2026-09-27-105 added declined any function with a receiver. Now a
/// method's params and an owned receiver's parts are adopted the same way
/// (`self.r`), with push, alias, tail, two-hop, fresh-temp and named-receiver
/// spellings alike.
#[test]
fn test_method_param_or_receiver_part_handed_over_on_one_path_runs_once() {
    let out = run(r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}{self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"n{n}" }; }
struct W { r: D, s: D, b: i64 }
fn mkw(n: i64) -> W { W { r: mkd(n), s: mkd(n + 100), b: n } }
fn keep(d: D) -> D { d }
struct T { w: W, t: D }
fn mkt(n: i64) -> T { T { w: mkw(n), t: mkd(n + 300) } }
struct H { k: i64 }
impl H {
    fn ret(ref self, w: W, c: bool) -> i64 { if c { let k = keep(w.r); return k.id; } 0 }
    fn push(ref self, w: W, c: bool) -> i64 { let mut xs: Vec[D] = Vec.new(); if c { xs.push(w.r); } xs.len() }
    fn al(ref self, w: W, c: bool) -> i64 { let r = w.r; if c { let k = keep(r); return k.id; } 0 }
    fn owns(self, w: W, c: bool) -> i64 { if c { let k = keep(w.r); return k.id; } self.k }
    fn two(ref self, a: i64, w: W, c: bool) -> i64 { if c { let k = keep(w.s); return k.id + a; } a }
}
impl W {
    fn ret(self, c: bool) -> i64 { if c { let k = keep(self.r); return k.id; } 0 }
    fn tail(self, c: bool) -> i64 { let mut xs: Vec[D] = Vec.new(); if c { xs.push(self.r) } xs.len() }
    fn al(self, c: bool) -> i64 { let r = self.r; if c { let k = keep(r); return k.id; } 0 }
    fn both(self, c: bool) -> D { if c { let k = keep(self.r); println(f"k{k.id}"); } self.s }
}
impl T { fn deep(self, c: bool) -> i64 { if c { let k = keep(self.w.r); return k.id; } 0 } }
fn main() {
    let h = H { k: 0 };
    println(f"a{h.ret(mkw(1), false)}"); println(f"a{h.ret(mkw(2), true)}")
    let w3 = mkw(3); println(f"b{h.push(w3, false)}"); let w4 = mkw(4); println(f"b{h.push(w4, true)}")
    println(f"c{h.al(mkw(5), false)}"); println(f"c{h.al(mkw(6), true)}")
    println(f"e{H { k: 7 }.owns(mkw(7), false)}"); println(f"e{H { k: 8 }.owns(mkw(8), true)}")
    println(f"f{h.two(1, mkw(9), false)}"); println(f"f{h.two(1, mkw(10), true)}")
    println(f"g{mkw(11).ret(false)}"); let w12 = mkw(12); println(f"g{w12.ret(true)}")
    println(f"i{mkw(13).tail(false)}"); println(f"i{mkw(14).tail(true)}")
    let w15 = mkw(15); println(f"j{w15.al(false)}"); println(f"j{mkw(16).al(true)}")
    let s17 = mkw(17).both(false); println(f"l{s17.id}"); let s18 = mkw(18).both(true); println(f"l{s18.id}")
    println(f"m{mkt(19).deep(false)}"); let t20 = mkt(20); println(f"m{t20.deep(true)}")
    println("end")
}"#);
    assert_eq!(out, "dD1n1\ndD101n101\na0\ndD2n2\ndD102n102\na2\ndD3n3\nb0\ndD103n103\ndD4n4\nb1\ndD104n104\ndD5n5\ndD105n105\nc0\ndD6n6\ndD106n106\nc6\ndD7n7\ndD107n107\ne7\ndD8n8\ndD108n108\ne8\ndD109n109\ndD9n9\nf1\ndD110n110\ndD10n10\nf111\ndD11n11\ndD111n111\ng0\ndD12n12\ng12\ndD112n112\ndD13n13\ndD113n113\ni0\ndD14n14\ndD114n114\ni1\ndD15n15\nj0\ndD115n115\ndD16n16\ndD116n116\nj16\ndD17n17\nl117\ndD117n117\nk18\ndD18n18\nl118\ndD118n118\ndD19n19\ndD319n319\ndD119n119\nm0\ndD20n20\nm20\ndD320n320\ndD120n120\nend\n");
}
