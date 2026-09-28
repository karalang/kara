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
fn e2e_self_field_handed_to_another_owner_runs_once() {
    let src = r#"struct D { id: i64, name: String }
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
}"#;
    let want = "d1n1\na1\nd101n101\nd2n2\nb1\nd102n102\nd3n3\nd103n103\nc3\nd4n4\nd104n104\ne1\nk5\nd5n5\nf105\nd105n105\nd6n6\nd306n306\nd106n106\ng6\nd7n7\nh7\nd307n307\nd107n107\nd108n108\nd8n8\ni8\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// B-2026-09-27-105 conditional cells — a by-value struct param whose `Drop`
/// field is handed to another owner on ONE path only (`if c { keep(w.r) }`,
/// `if c { xs.push(w.r) }`, a tail push, a `let r = w.r` alias, a `match` arm,
/// two projection hops, a forwarding caller, an associated fn). The caller
/// masked the part on every path while the callee ran it on none, so the
/// path that kept the part lost its body; the callee now adopts the part under
/// a per-path flag cleared at the handing statement.
#[test]
fn e2e_param_field_handed_over_on_one_path_runs_once() {
    let src = r#"struct D { id: i64, name: String }
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
    println(f"a{ret(mkw(1), false)}"); println(f"a{ret(mkw(2), true)}")
    println(f"b{push(mkw(3), false)}"); println(f"b{push(mkw(4), true)}")
    println(f"c{tailpush(mkw(5), false)}"); println(f"c{tailpush(mkw(6), true)}")
    println(f"e{viaalias(mkw(7), false)}"); println(f"e{viaalias(mkw(8), true)}")
    println(f"f{viaaliaspush(mkw(9), false)}"); println(f"f{viaaliaspush(mkw(10), true)}")
    println(f"g{arm(mkw(11), false)}"); println(f"g{arm(mkw(12), true)}")
    println(f"h{twohop(mkx(13), false)}"); println(f"h{twohop(mkx(14), true)}")
    println(f"i{fwd(mkw(15), false)}"); println(f"i{fwd(mkw(16), true)}")
    println(f"j{W.assoc(mkw(17), false)}"); println(f"j{W.assoc(mkw(18), true)}")
    println("end")
}"#;
    let want = "d1n1\nd101n101\na0\nd2n2\nd102n102\na2\nd3n3\nd103n103\nb0\nd4n4\nd104n104\nb1\nd5n5\nd105n105\nc0\nd6n6\nd106n106\nc1\nd7n7\nd107n107\ne0\nd8n8\nd108n108\ne8\nd9n9\nd109n109\nf0\nd10n10\nd110n110\nf1\nd11n11\nd111n111\ng0\nd12n12\nd112n112\ng12\nd13n13\nd313n313\nd113n113\nh0\nd14n14\nd314n314\nd114n114\nh14\nd15n15\nd115n115\ni0\nd16n16\nd116n116\ni16\nd17n17\nd117n117\nj0\nd18n18\nd118n118\nj18\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
