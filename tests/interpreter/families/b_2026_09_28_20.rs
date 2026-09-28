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

/// B-2026-09-28-20 cell (e) — a by-value USER-enum param stored on only some
/// paths (`fn csg(v: mut ref Vec[G], x: G, c: bool) { if c { v.push(x); } }`,
/// `enum G { A(D), B }`) ran no payload body on the path that did not store
/// it, on all four surfaces and for every argument spelling; handed a field
/// (`csg(mut xs, v.g, true)`), the caller's walk ran it beside the vector's
/// drain on the storing path. The callee now adopts the payload bodies under
/// the per-path flag the storing statement clears, and the caller stands a
/// projection argument down, as both already did for a conditional return.
#[test]
fn test_enum_param_stored_on_one_path_runs_its_payload_once() {
    let out = run(r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}{self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"n{n}" }; }
struct W { r: D, s: D, b: i64 }
fn mkw(n: i64) -> W { W { r: mkd(n), s: mkd(n + 100), b: n } }
fn keep(d: D) -> D { d }
enum G { A(D), B }
struct V { g: G, n: i64 }
fn mkv(n: i64) -> V { V { g: G.A(mkd(n)), n: n } }
struct U { v: V }
fn csg(v: mut ref Vec[G], x: G, c: bool) { if c { v.push(x); } }
fn tl(v: mut ref Vec[G], x: G, c: bool) { if c { v.push(x) } }
fn loc(x: G, c: bool) -> i64 { let mut ys: Vec[G] = Vec.new(); if c { ys.push(x); } ys.len() }
struct H { k: i64 }
impl H { fn m(ref self, v: mut ref Vec[G], x: G, c: bool) { if c { v.push(x); } } }
fn main() {
    let mut xs: Vec[G] = Vec.new();
    csg(mut xs, mkv(1).g, false); csg(mut xs, mkv(2).g, true); println(f"a{xs.len()}")
    tl(mut xs, G.A(mkd(3)), false); let g4 = G.A(mkd(4)); tl(mut xs, g4, true); println(f"b{xs.len()}")
    println(f"c{loc(G.A(mkd(5)), false)}"); let v6 = mkv(6); println(f"c{loc(v6.g, true)}")
    let h = H { k: 0 }; let v7 = mkv(7); h.m(mut xs, v7.g, false); h.m(mut xs, G.A(mkd(8)), true); println(f"e{xs.len()}")
    let u9 = U { v: mkv(9) }; csg(mut xs, u9.v.g, false); let u10 = U { v: mkv(10) }; csg(mut xs, u10.v.g, true); println(f"f{xs.len()}")
    csg(mut xs, G.B, false); csg(mut xs, G.B, true); println(f"g{xs.len()}")
    println("end")
}"#);
    assert_eq!(out, "dD1n1\na1\ndD3n3\nb2\ndD5n5\nc0\ndD6n6\nc1\ndD7n7\ne3\ndD9n9\nf4\ng5\ndD2n2\ndD4n4\ndD8n8\ndD10n10\nend\n");
}

/// B-2026-09-28-20 cell (i) — a by-value struct with NO `Drop` of its own
/// whose fields run user bodies (`struct W { r: D, s: D, b: i64 }`), stored on
/// only some paths (`if c { xs.push(w); }`), lost its fields' bodies on the
/// path that did not store it (a fresh temporary on all four surfaces, a named
/// argument under `--interp`) and ran them twice on the path that did (a named
/// or projected argument, compiled). The callee now adopts the fields' bodies
/// under the per-path store flag and the caller stands its field walk down on
/// every path: free function, method, tail push, loop, nested struct.
#[test]
fn test_field_bodies_struct_stored_on_one_path_runs_its_fields_once() {
    let out = run(r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}{self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"n{n}" }; }
struct W { r: D, s: D, b: i64 }
fn mkw(n: i64) -> W { W { r: mkd(n), s: mkd(n + 100), b: n } }
fn keep(d: D) -> D { d }
fn ownw(w: W, c: bool) -> i64 { let mut xs: Vec[W] = Vec.new(); if c { xs.push(w); } xs.len() }
fn st(xs: mut ref Vec[W], w: W, c: bool) { if c { xs.push(w); } }
fn tl(w: W, c: bool) -> i64 { let mut xs: Vec[W] = Vec.new(); if c { xs.push(w) } xs.len() }
fn lst(xs: mut ref Vec[W], w: W, n: i64) { let mut i = 0; while i < n { if i == 1 { xs.push(w); return; } i = i + 1; } }
struct H { k: i64 }
impl H { fn m(ref self, w: W, c: bool) -> i64 { let mut xs: Vec[W] = Vec.new(); if c { xs.push(w); } xs.len() } }
struct X { w: W, t: D }
struct Y { w: W, n: i64 }
fn yst(xs: mut ref Vec[Y], y: Y, c: bool) { if c { xs.push(y); } }
fn main() {
    println(f"a{ownw(mkw(1), false)}"); println(f"a{ownw(mkw(2), true)}")
    let w3 = mkw(3); println(f"b{ownw(w3, false)}"); let w4 = mkw(4); println(f"b{ownw(w4, true)}")
    let mut ws: Vec[W] = Vec.new(); st(mut ws, mkw(5), false); let w6 = mkw(6); st(mut ws, w6, true); println(f"c{ws.len()}")
    println(f"d{tl(mkw(7), false)}"); println(f"d{tl(mkw(8), true)}")
    let h = H { k: 0 }; println(f"e{h.m(mkw(9), false)}"); let w10 = mkw(10); println(f"e{h.m(w10, true)}")
    let x11 = X { w: mkw(11), t: mkd(311) }; println(f"f{ownw(x11.w, false)}"); let x12 = X { w: mkw(12), t: mkd(312) }; println(f"f{ownw(x12.w, true)}")
    let mut ys: Vec[Y] = Vec.new(); yst(mut ys, Y { w: mkw(13), n: 13 }, false); let y14 = Y { w: mkw(14), n: 14 }; yst(mut ys, y14, true); println(f"g{ys.len()}")
    lst(mut ws, mkw(15), 1); let w16 = mkw(16); lst(mut ws, w16, 3); println(f"h{ws.len()}")
    println("end")
}"#);
    assert_eq!(out, "dD101n101\ndD1n1\na0\ndD102n102\ndD2n2\na1\ndD103n103\ndD3n3\nb0\ndD104n104\ndD4n4\nb1\ndD105n105\ndD5n5\nc1\ndD107n107\ndD7n7\nd0\ndD108n108\ndD8n8\nd1\ndD109n109\ndD9n9\ne0\ndD110n110\ndD10n10\ne1\ndD111n111\ndD11n11\nf0\ndD311n311\ndD112n112\ndD12n12\nf1\ndD312n312\ndD113n113\ndD13n13\ng1\ndD114n114\ndD14n14\ndD115n115\ndD15n15\nh2\ndD106n106\ndD6n6\ndD116n116\ndD16n16\nend\n");
}
