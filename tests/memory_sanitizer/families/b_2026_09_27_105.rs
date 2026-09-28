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
