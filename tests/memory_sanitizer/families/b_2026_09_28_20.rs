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
fn asan_method_param_or_receiver_part_handed_over_on_one_path_is_freed_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
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
}
"#,
        &[
            "dD1n1",
            "dD101n101",
            "a0",
            "dD2n2",
            "dD102n102",
            "a2",
            "dD3n3",
            "b0",
            "dD103n103",
            "dD4n4",
            "b1",
            "dD104n104",
            "dD5n5",
            "dD105n105",
            "c0",
            "dD6n6",
            "dD106n106",
            "c6",
            "dD7n7",
            "dD107n107",
            "e7",
            "dD8n8",
            "dD108n108",
            "e8",
            "dD109n109",
            "dD9n9",
            "f1",
            "dD110n110",
            "dD10n10",
            "f111",
            "dD11n11",
            "dD111n111",
            "g0",
            "dD12n12",
            "g12",
            "dD112n112",
            "dD13n13",
            "dD113n113",
            "i0",
            "dD14n14",
            "dD114n114",
            "i1",
            "dD15n15",
            "j0",
            "dD115n115",
            "dD16n16",
            "dD116n116",
            "j16",
            "dD17n17",
            "l117",
            "dD117n117",
            "k18",
            "dD18n18",
            "l118",
            "dD118n118",
            "dD19n19",
            "dD319n319",
            "dD119n119",
            "m0",
            "dD20n20",
            "m20",
            "dD320n320",
            "dD120n120",
            "end",
        ],
        "asan_method_param_or_receiver_part_handed_over_on_one_path_is_freed_once",
        80,
    );
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
fn asan_enum_param_stored_on_one_path_frees_its_payload_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
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
}
"#,
        &[
            "dD1n1", "a1", "dD3n3", "b2", "dD5n5", "c0", "dD6n6", "c1", "dD7n7", "e3", "dD9n9",
            "f4", "g5", "dD2n2", "dD4n4", "dD8n8", "dD10n10", "end",
        ],
        "asan_enum_param_stored_on_one_path_frees_its_payload_once",
        30,
    );
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
fn asan_field_bodies_struct_stored_on_one_path_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
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
}
"#,
        &[
            "dD101n101",
            "dD1n1",
            "a0",
            "dD102n102",
            "dD2n2",
            "a1",
            "dD103n103",
            "dD3n3",
            "b0",
            "dD104n104",
            "dD4n4",
            "b1",
            "dD105n105",
            "dD5n5",
            "c1",
            "dD107n107",
            "dD7n7",
            "d0",
            "dD108n108",
            "dD8n8",
            "d1",
            "dD109n109",
            "dD9n9",
            "e0",
            "dD110n110",
            "dD10n10",
            "e1",
            "dD111n111",
            "dD11n11",
            "f0",
            "dD311n311",
            "dD112n112",
            "dD12n12",
            "f1",
            "dD312n312",
            "dD113n113",
            "dD13n13",
            "g1",
            "dD114n114",
            "dD14n14",
            "dD115n115",
            "dD15n15",
            "h2",
            "dD106n106",
            "dD6n6",
            "dD116n116",
            "dD16n16",
            "end",
        ],
        "asan_field_bodies_struct_stored_on_one_path_frees_once",
        60,
    );
}
