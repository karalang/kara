//! B-2026-09-27-102 — a boxed generic enum param handed back on one path and forwarded on the other.

use super::*;

/// B-2026-09-27-102 — `fn pk(h: Ho[String], k: bool) -> Ho[String] { if k {
/// return h } showt(h); return Ho.Empty }`. The caller keeps its binding
/// across the call because the callee MAY hand the box back, and the
/// post-call compare settles which leg ran. The forwarding leg had to leave
/// the box with the caller too, and before this fix it did not: the callee
/// handed its param to `showt`, which freed it, and the caller freed it again.
/// Free, forwarded-through-a-helper (`pkc`), static and instance-method
/// spellings, each on both legs, plus both legs from a `for` loop view.
#[test]
fn interp_boxed_generic_enum_param_handed_back_or_forwarded_by_path() {
    let out = run(r#"enum Ho[T] { Full(T), Empty }
struct K { k: i64 }
fn showt(h: Ho[String]) { match h { Ho.Full(r) => println(f"t{r}"), Ho.Empty => println("e") } }
fn keep(h: Ho[String]) -> Ho[String] { h }
fn fwd(h: Ho[String]) { showt(h) }
fn mk(s: String) -> Ho[String] { return Ho.Full(s + "!") }
fn pk(h: Ho[String], k: bool) -> Ho[String] { if k { return h } showt(h); return Ho.Empty }
fn pkc(h: Ho[String], k: bool) -> Ho[String] { if k { return keep(h) } fwd(h); return Ho.Empty }
impl K {
    fn pks(h: Ho[String], k: bool) -> Ho[String] { if k { return h } showt(h); return Ho.Empty }
    fn pkm(self, h: Ho[String], k: bool) -> Ho[String] { if k { return h } showt(h); return Ho.Empty }
}
fn main() {
    println("free");
    let a = mk("a"); let ra = pk(a, false); showt(ra);
    let b = mk("b"); let rb = pk(b, true); showt(rb);
    println("chain");
    let c = mk("c"); let rc = pkc(c, false); showt(rc);
    let d = mk("d"); let rd = pkc(d, true); showt(rd);
    println("static");
    let e = mk("e"); let re = K.pks(e, false); showt(re);
    let f = mk("f"); let rf = K.pks(f, true); showt(rf);
    println("method");
    let g = mk("g"); let kg = K { k: 1 }; let rg = kg.pkm(g, false); showt(rg);
    let h = mk("h"); let kh = K { k: 2 }; let rh = kh.pkm(h, true); showt(rh);
    println("loop");
    let mut v: Vec[Ho[String]] = Vec.new();
    v.push(mk("x"));
    v.push(Ho.Empty);
    for x in v { let r = pk(x, false); showt(r) }
    for x in v { let r = pkc(x, true); showt(r) }
    println(f"n{v.len()}");
    println("end")
}
"#);
    assert_eq!(out, "free\nta!\ne\ntb!\nchain\ntc!\ne\ntd!\nstatic\nte!\ne\ntf!\nmethod\ntg!\ne\nth!\nloop\ntx!\ne\ne\ne\ntx!\ne\nn2\nend\n");
}
