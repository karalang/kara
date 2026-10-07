//! B-2026-09-29-79 -- a nested struct field returned out of a caller-retained
//! by-value param leaves as a clone with its `shared` values retained.

use super::*;

/// B-2026-09-29-79 -- a nested struct field handed out of a by-value struct
/// param that holds a `shared` value (`fn f(q: W) -> S { return q.u }` over
/// `W { u: S }` and `S { h: Sh, id: i64 }`). Such a param is caller-retained:
/// the callee registers no drop for it and the caller frees the argument, `u`
/// included. The returned words aliased that heap, so the caller's result read
/// freed memory (a garbage `h.k`, an abort in `malloc` under `karac run`) and a
/// `String` beside the `shared` field was freed twice. Covers `return q.u`, the
/// tail `q.u`, `let x = q.u; return x`, the same after a `..` destructure, a
/// field with no `Drop`, one with a `String`, a param that only reads the field
/// (which also leaked a copy on the old tree), a conditional return, a
/// shadowed `x`, and an owned `self` method.
#[test]
fn asan_caller_retained_param_returns_cloned_struct_field() {
    assert_clean_asan_run(
        r#"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mk(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
struct S4 { h: Sh, id: i64 }
struct S5 { h: Sh, t: String, id: i64 }
impl Drop for S5 { fn drop(mut ref self) { println(f"dT{self.id}") } }
struct W3 { u: S2, n: i64 }
struct W6 { u: S4, n: i64 }
struct W7 { u: S5, n: i64 }
impl W3 { fn go(self) -> S2 { let x = self.u; println(f"n{self.n}"); return x } }
fn f1(q: W3) -> S2 { return q.u }
fn f2(q: W3) -> S2 { q.u }
fn f3(q: W3) -> S2 { let x = q.u; return x }
fn f4(q: W3) -> S2 { let x = q.u; let W3 { n, .. } = q; println(f"n{n}"); return x }
fn f5(q: W6) -> S4 { return q.u }
fn f6(q: W7) -> S5 { return q.u }
fn f7(q: W7) -> S5 { let x = q.u; let W7 { n, .. } = q; println(f"n{n}"); return x }
fn f8(q: W7) -> i64 { let x = q.u; let W7 { n, .. } = q; println(f"x{x.t.len()}"); return n }
fn f9(q: W3, b: bool) -> S2 { if b { return q.u } return mk(4) }
fn f10(q: W3) -> S2 { let x = q.u; if q.n > 1 { return x } mk(5) }
fn f11(q: W3) -> S2 { let x = q.u; let x = mk(3); return x }
fn main() {
  println("-c1"); let a = f1(W3 { u: mk(9), n: 2 }); println(f"r{a.id}{a.h.k}");
  println("-c2"); let b = f2(W3 { u: mk(8), n: 2 }); println(f"r{b.id}{b.h.k}");
  println("-c3"); let c = f3(W3 { u: mk(7), n: 2 }); println(f"r{c.id}{c.h.k}");
  println("-c4"); let d = f4(W3 { u: mk(6), n: 2 }); println(f"r{d.id}{d.h.k}");
  println("-c5"); let e = f5(W6 { u: S4 { h: Sh { k: 5 }, id: 9 }, n: 2 }); println(f"r{e.id}{e.h.k}");
  println("-c6"); let g = f6(W7 { u: S5 { h: Sh { k: 5 }, t: f"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa{1}", id: 9 }, n: 2 }); println(f"r{g.id}{g.h.k}{g.t.len()}");
  println("-c7"); let h = f7(W7 { u: S5 { h: Sh { k: 4 }, t: f"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbb{2}", id: 8 }, n: 3 }); println(f"r{h.id}{h.h.k}{h.t.len()}");
  println("-c8"); println(f"r{f8(W7 { u: S5 { h: Sh { k: 3 }, t: f"cccccccccccccccccccccccccccccc{3}", id: 7 }, n: 4 })}");
  println("-c9"); let i = f9(W3 { u: mk(9), n: 2 }, true); println(f"r{i.id}"); let j = f9(W3 { u: mk(8), n: 2 }, false); println(f"r{j.id}");
  println("-c10"); let k = f10(W3 { u: mk(9), n: 2 }); println(f"r{k.id}"); let l = f10(W3 { u: mk(8), n: 0 }); println(f"r{l.id}");
  println("-c11"); let m = f11(W3 { u: mk(9), n: 2 }); println(f"r{m.id}");
  println("-c12"); let w = W3 { u: mk(9), n: 2 }; let o = w.go(); println(f"r{o.id}{o.h.k}");
  println("end")
}
"#,
        &[
            "-c1", "r99", "dS9", "-c2", "r88", "dS8", "-c3", "r77", "dS7", "-c4", "n2", "r66",
            "dS6", "-c5", "r95", "-c6", "r9531", "dT9", "-c7", "n3", "r8431", "dT8", "-c8", "x31",
            "dT7", "r4", "-c9", "r9", "dS9", "r4", "dS4", "-c10", "r9", "dS9", "r5", "dS5", "-c11",
            "dS9", "r3", "dS3", "-c12", "n2", "r99", "dS9", "end",
        ],
        "caller_retained_param_returns_cloned_struct_field",
    );
}
