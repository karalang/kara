//! B-2026-09-28-56 — a by-value param handed back on some exits and only
//! BORROWED on another (`peek(r)` over `fn peek(x: ref R)`) runs its `Drop`
//! body once on each path.

use super::*;

/// B-2026-09-28-56 — `fn p4(r: R, f: bool) -> R { if f { return r } return R {
/// name: f"z", id: peek(r) } }` printed no body for `r` on the path where it
/// dies inside, on all four surfaces: the borrow read as a hand-over, both in
/// the conditional-return predicate's leaf test and in both backends' per-path
/// disarm. Neighbours: a borrow as a statement before or after the branch, in
/// a `let`, nested in a call argument or a constructor, a `ref self` / `mut
/// ref self` method on the param, a `mut ref` param, an `Option` param, the
/// method and associated positions of the callee, and a named argument.
#[test]
fn e2e_param_borrowed_on_the_path_that_keeps_it_runs_its_body_once() {
    let src = r#"struct R { name: String, id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.name}") } }
impl R { fn peekm(ref self) -> i64 { return self.id; } fn bump(mut ref self) { self.id = self.id + 100; } }
fn mk(n: i64, s: String) -> R { R { name: s, id: n } }
fn peek(x: ref R) -> i64 { return x.id; }
fn look(x: ref R) { println(f"L{x.id}") }
fn poke(x: mut ref R) { x.id = x.id + 1; }
fn lenr(x: ref Option[R]) -> i64 { 5 }
fn p4(r: R, f: bool) -> R { if f { return r } return R { name: f"z", id: peek(r) } }
fn p5(r: R, f: bool) -> R { if f { return r } look(r); return mk(5, f"y") }
fn p6(r: R, f: bool) -> R { if f { return r } let k = peek(r); return mk(k, f"x") }
fn p7(r: R, f: bool) -> R { if f { return r } return mk(peek(r), f"w") }
fn p8(r: R, f: bool) -> R { if f { return r } return R { name: f"v", id: r.peekm() } }
fn p9(r: R, f: bool) -> R { look(r); if f { return r } return mk(9, f"u") }
fn c1(r: R, f: bool) -> R { if f { return r } poke(mut r); return mk(r.id, f"t") }
fn c2(r: R, f: bool) -> R { if f { return r } r.bump(); return mk(1, f"s") }
fn c3(o: Option[R], f: bool) -> Option[R] { if f { return o } return Some(mk(lenr(o), f"o")) }
struct Q { z: i64 }
impl Q {
    fn m4(ref self, r: R, f: bool) -> R { if f { return r } return mk(peek(r), f"q") }
    fn a4(r: R, f: bool) -> R { if f { return r } return mk(peek(r), f"a") }
}
fn main() {
    let q = Q { z: 0 };
    let a = p4(mk(1, f"a"), false); println(f"k{a.id}");
    let b = p4(mk(2, f"b"), true); println(f"k{b.id}");
    let c = p5(mk(3, f"c"), false); println(f"k{c.id}");
    let d = p6(mk(4, f"d"), false); println(f"k{d.id}");
    let e = p7(mk(6, f"e"), false); println(f"k{e.id}");
    let g = p8(mk(7, f"g"), false); println(f"k{g.id}");
    let h = p8(mk(8, f"h"), true); println(f"k{h.id}");
    let i = p9(mk(10, f"i"), false); println(f"k{i.id}");
    let j = p9(mk(11, f"j"), true); println(f"k{j.id}");
    let l = c1(mk(12, f"l"), false); println(f"k{l.id}");
    let m = c2(mk(14, f"m"), false); println(f"k{m.id}");
    let n = c3(Some(mk(15, f"n")), false); println("k15");
    let o = q.m4(mk(16, f"o"), false); println(f"k{o.id}");
    let p = q.m4(mk(17, f"p"), true); println(f"k{p.id}");
    let r = Q.a4(mk(18, f"r"), false); println(f"k{r.id}");
    let x = mk(19, f"x"); let s = p7(x, false); println(f"k{s.id}");
    println("end")
}"#;
    let want = "dR1/a\nk1\ndR1/z\nk2\ndR2/b\nL3\ndR3/c\nk5\ndR5/y\ndR4/d\nk4\ndR4/x\ndR6/e\nk6\ndR6/w\ndR7/g\nk7\ndR7/v\nk8\ndR8/h\nL10\ndR10/i\nk9\ndR9/u\nL11\nk11\ndR11/j\ndR13/l\nk13\ndR13/t\ndR114/m\nk1\ndR1/s\ndR15/n\ndR5/o\nk15\ndR16/o\nk16\ndR16/q\nk17\ndR17/p\ndR18/r\nk18\ndR18/a\ndR19/x\nk19\ndR19/w\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
