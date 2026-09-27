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
fn asan_boxed_generic_enum_param_handed_back_or_forwarded_by_path() {
    assert_clean_asan_run(
        r#"enum Ho[T] { Full(T), Empty }
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
"#,
        &[
            "free", "ta!", "e", "tb!", "chain", "tc!", "e", "td!", "static", "te!", "e", "tf!",
            "method", "tg!", "e", "th!", "loop", "tx!", "e", "e", "e", "tx!", "e", "n2", "end",
        ],
        "asan_boxed_generic_enum_param_handed_back_or_forwarded_by_path",
    );
}

/// B-2026-09-27-102 — the row's own cells, with a `Drop` payload: the body
/// runs once, inside the callee on the forwarding leg and at the result's
/// death on the hand-back leg. Compiled surfaces only: `--interp` omits the
/// body on the forwarding leg (`s1abcd e` where `s1abcd dS1 e` is right),
/// filed as B-2026-09-27-96.
#[test]
fn asan_boxed_generic_enum_drop_param_handed_back_or_forwarded_by_path() {
    assert_clean_asan_run(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
enum Ho[T] { Full(T), Empty }
fn mks(i: i64) -> S { return S { id: i, s: "ab".to_string() + "cd" } }
fn shows(h: Ho[S]) { match h { Ho.Full(r) => println(f"s{r.id}{r.s}"), Ho.Empty => println("e") } }
fn fwd(h: Ho[S]) { shows(h) }
fn pkf(h: Ho[S], k: bool) -> Ho[S] { if k { return h } shows(h); return Ho.Empty }
fn pk3(h: Ho[S], k: bool) -> Ho[S] { if k { return h } fwd(h); return Ho.Empty }
struct K { k: i64 }
impl K {
    fn pk(h: Ho[S], k: bool) -> Ho[S] { if k { return h } shows(h); return Ho.Empty }
    fn pkm(self, h: Ho[S], k: bool) -> Ho[S] { if k { return h } shows(h); return Ho.Empty }
}
fn main() {
    let a: Ho[S] = Ho.Full(mks(1)); let ra = pkf(a, false); shows(ra);
    let b: Ho[S] = Ho.Full(mks(2)); let rb = pkf(b, true); shows(rb);
    let c: Ho[S] = Ho.Full(mks(3)); let rc = pk3(c, false); shows(rc);
    let d: Ho[S] = Ho.Full(mks(4)); let rd = K.pk(d, false); shows(rd);
    let e: Ho[S] = Ho.Full(mks(5)); let ke = K { k: 1 }; let re = ke.pkm(e, false); shows(re);
    let f: Ho[S] = Ho.Full(mks(6)); let kf = K { k: 2 }; let rf = kf.pkm(f, true); shows(rf);
    println("end")
}
"#,
        &[
            "s1abcd", "dS1", "e", "s2abcd", "dS2", "s3abcd", "dS3", "e", "s4abcd", "dS4", "e",
            "s5abcd", "dS5", "e", "s6abcd", "dS6", "end",
        ],
        "asan_boxed_generic_enum_drop_param_handed_back_or_forwarded_by_path",
    );
}
