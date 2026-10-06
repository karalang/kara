//! B-2026-10-05-121: a `shared` value held in a by-value method argument or receiver is released at the call

use super::*;

/// B-2026-10-05-121 — design.md § Drop ordering rule 3: a by-value argument
/// the callee keeps dies when the call returns. B-2026-10-04-47 moved that
/// release to the call for a FREE function's argument; a method's owned
/// receiver (`v.g2()` over `fn g2(self)`, a trait method, a tuple-holding
/// struct) and a method's by-value argument (`g.m(o)`, `g.mw(w)`) still
/// released the `shared` value they hold at the caller's scope exit compiled,
/// and the `Option` argument at scope exit under `--interp` too. A method that
/// stores the argument (`put`) or hands it back (`pass`) keeps it alive, and
/// the receiver's type picks the method (`G2.m` beside `G.m`).
#[test]
fn e2e_method_receiver_and_argument_shared_release_at_the_call() {
    let Some(out) = run_program(
        r#"shared struct H { id: i64 }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
struct W { o: Option[H], n: i64 }
struct D { h: H, n: i64 }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.n}") } }
struct T { h: (H, i64) }
struct G { n: i64, keep: Option[H] }
struct G2 { n: i64 }
trait Eat { fn eat(self); }
impl W {
    fn g2(self) { println(f"g{self.n}") }
    fn me(self) -> W { self }
    fn into_g(self) -> G { G { n: 1, keep: self.o } }
}
impl D { fn g2(self) { println(f"gd{self.n}") } }
impl T { fn g2(self) { println("gt") } }
impl Eat for W { fn eat(self) { println("eat") } }
impl G {
    fn m(ref self, o: Option[H]) { println("m") }
    fn mw(ref self, w: W) { println("mw") }
    fn put(mut ref self, o: Option[H]) { self.keep = o; println("put") }
    fn pass(ref self, o: Option[H]) -> Option[H] { o }
    fn md(ref self, d: D) { println("md") }
    fn mh(ref self, h: H) { println("mh") }
}
impl G2 { fn m(ref self, o: Option[H]) { println("m2") } }
fn mkw(i: i64) -> W { W { o: Some(H { id: i }), n: i } }
fn mkg() -> G { G { n: 0, keep: None } }
fn main() {
    { let v = mkw(1); v.g2(); println("_1"); }
    { let v: W = mkw(2); v.g2(); println("_2"); }
    { let g = mkg(); let o = Some(H { id: 5 }); g.m(o); println("_5"); }
    { let mut g = mkg(); let o = Some(H { id: 6 }); g.put(o); println("_6"); println(f"{g.n}"); }
    { let g = mkg(); let o = Some(H { id: 7 }); let p = g.pass(o); println("_7"); println(f"{p.is_some()}"); }
    { let v = T { h: (H { id: 11 }, 1) }; v.g2(); println("_11"); }
    { let v = mkw(12); v.eat(); println("_12"); }
    { let g = G2 { n: 0 }; let o = Some(H { id: 13 }); g.m(o); println("_13"); }
    { let g = mkg(); let o = Some(H { id: 14 }); if true { g.m(o); } println("_14"); }
    { let v = mkw(16); let c = true; if c { v.g2(); } else { println("no"); } println("_16"); }
    { let g = mkg(); let w = mkw(17); g.mw(w); println("_17"); }
    { let g = mkg(); let o = Some(H { id: 18 }); let o2 = Some(H { id: 19 }); g.m(o); g.m(o2); println("_18"); }
    { let mut v = mkw(20); v.n = 21; v.g2(); println("_19"); }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "g1\ndH1\n_1\ng2\ndH2\n_2\nm\ndH5\n_5\nput\n_6\n0\ndH6\n_7\ntrue\ndH7\ngt\ndH11\n_11\neat\ndH12\n_12\nm2\ndH13\n_13\nm\ndH14\n_14\ng16\ndH16\n_16\nmw\ndH17\n_17\nm\ndH18\nm\ndH19\n_18\ng21\ndH20\n_19\nend\n", "got:\n{out}");
}
