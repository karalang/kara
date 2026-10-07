//! B-2026-09-27-105 — an OWNED-`self` method that hands a `Drop` field of
//! `self` to another owner (`keep(self.r)`, `xs.push(self.r)`) runs that
//! field's body once, for a named and a fresh-temp receiver alike.

use super::*;

/// B-2026-09-27-105 cell (f) — `fn take(self) -> i64 { let k = keep(self.r);
/// k.id }` ran `self.r`'s body in the callee's `k` and again in the caller's
/// walk of the receiver, on all four surfaces: the part scan left `self`-rooted
/// hand-overs out, because a fresh-temp receiver's registrar walked the whole
/// value beside the part channel. The call site now masks that walk in place.
/// `i` pins the neighbour whose registrar walk must stay whole.
#[test]
fn asan_self_field_handed_to_another_owner_is_freed_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"d{self.id}{self.name}") } }
fn mkd(n: i64) -> D { D { id: n, name: f"n{n}" } }
fn keep(d: D) -> D { d }
struct W { r: D, s: D, b: i64 }
fn mkw(n: i64) -> W { W { r: mkd(n), s: mkd(n + 100), b: n } }
struct T { w: W, t: D }
fn mkt(n: i64) -> T { T { w: mkw(n), t: mkd(n + 300) } }
impl W {
    fn take(self) -> i64 { let k = keep(self.r); k.id }
    fn pushr(self) -> i64 { let mut xs: Vec[D] = Vec.new(); xs.push(self.r); xs.len() }
    fn both(self) -> D { let k = keep(self.r); println(f"k{k.id}"); self.s }
    fn getb(self) -> i64 { self.b }
}
impl T {
    fn deep(self) -> i64 { let k = keep(self.w.r); k.id }
}
fn main() {
    let w = mkw(1);
    println(f"a{w.take()}");
    let v = mkw(2);
    println(f"b{v.pushr()}");
    println(f"c{mkw(3).take()}");
    println(f"e{mkw(4).pushr()}");
    let s = mkw(5).both();
    println(f"f{s.id}");
    println(f"g{mkt(6).deep()}");
    let t = mkt(7);
    println(f"h{t.deep()}");
    println(f"i{mkw(8).getb()}");
    println("end")
}
"#,
        &[
            "d1n1", "a1", "d101n101", "d2n2", "b1", "d102n102", "d3n3", "d103n103", "c3", "d4n4",
            "d104n104", "e1", "k5", "d5n5", "f105", "d105n105", "d6n6", "d306n306", "d106n106",
            "g6", "d7n7", "h7", "d307n307", "d107n107", "d108n108", "d8n8", "i8", "end",
        ],
        "asan_self_field_handed_to_another_owner_is_freed_once",
        40,
    );
}

/// B-2026-09-27-105 conditional cells — a by-value struct param whose `Drop`
/// field is handed to another owner on ONE path only (`if c { keep(w.r) }`,
/// `if c { xs.push(w.r) }`, a tail push, a `let r = w.r` alias, a `match` arm,
/// two projection hops, a forwarding caller, an associated fn). The caller
/// masked the part on every path while the callee ran it on none, so the
/// path that kept the part lost its body; the callee now adopts the part under
/// a per-path flag cleared at the handing statement.
#[test]
fn asan_param_field_handed_over_on_one_path_is_freed_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"d{self.id}{self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"n{n}" }; }
struct W { r: D, s: D, b: i64 }
fn mkw(n: i64) -> W { W { r: mkd(n), s: mkd(n + 100), b: n } }
struct X { w: W, t: D }
fn mkx(n: i64) -> X { X { w: mkw(n), t: mkd(n + 300) } }
fn keep(d: D) -> D { d }
fn ret(w: W, c: bool) -> i64 { if c { let k = keep(w.r); return k.id; } 0 }
fn push(w: W, c: bool) -> i64 { let mut xs: Vec[D] = Vec.new(); if c { xs.push(w.r); } xs.len() }
fn tailpush(w: W, c: bool) -> i64 { let mut xs: Vec[D] = Vec.new(); if c { xs.push(w.r) } xs.len() }
fn viaalias(w: W, c: bool) -> i64 { let r = w.r; if c { let k = keep(r); return k.id; } 0 }
fn viaaliaspush(w: W, c: bool) -> i64 { let r = w.r; let mut xs: Vec[D] = Vec.new(); if c { xs.push(r); } xs.len() }
fn arm(w: W, c: bool) -> i64 { match c { true => { let k = keep(w.r); k.id } false => 0 } }
fn twohop(x: X, c: bool) -> i64 { if c { let k = keep(x.w.r); return k.id; } 0 }
fn fwd(w: W, c: bool) -> i64 { ret(w, c) }
impl W { fn assoc(w: W, c: bool) -> i64 { if c { let k = keep(w.r); return k.id; } 0 } }
fn main() {
    println(f"a{ret(mkw(1), false)}"); println(f"a{ret(mkw(2), true)}");
    println(f"b{push(mkw(3), false)}"); println(f"b{push(mkw(4), true)}");
    println(f"c{tailpush(mkw(5), false)}"); println(f"c{tailpush(mkw(6), true)}");
    println(f"e{viaalias(mkw(7), false)}"); println(f"e{viaalias(mkw(8), true)}");
    println(f"f{viaaliaspush(mkw(9), false)}"); println(f"f{viaaliaspush(mkw(10), true)}");
    println(f"g{arm(mkw(11), false)}"); println(f"g{arm(mkw(12), true)}");
    println(f"h{twohop(mkx(13), false)}"); println(f"h{twohop(mkx(14), true)}");
    println(f"i{fwd(mkw(15), false)}"); println(f"i{fwd(mkw(16), true)}");
    println(f"j{W.assoc(mkw(17), false)}"); println(f"j{W.assoc(mkw(18), true)}");
    println("end")
}
"#,
        &[
            "d1n1", "d101n101", "a0", "d2n2", "d102n102", "a2", "d3n3", "d103n103", "b0", "d4n4",
            "d104n104", "b1", "d5n5", "d105n105", "c0", "d6n6", "d106n106", "c1", "d7n7",
            "d107n107", "e0", "d8n8", "d108n108", "e8", "d9n9", "d109n109", "f0", "d10n10",
            "d110n110", "f1", "d11n11", "d111n111", "g0", "d12n12", "d112n112", "g12", "d13n13",
            "d313n313", "d113n113", "h0", "d14n14", "d314n314", "d114n114", "h14", "d15n15",
            "d115n115", "i0", "d16n16", "d116n116", "i16", "d17n17", "d117n117", "j0", "d18n18",
            "d118n118", "j18", "end",
        ],
        "asan_param_field_handed_over_on_one_path_is_freed_once",
        70,
    );
}
