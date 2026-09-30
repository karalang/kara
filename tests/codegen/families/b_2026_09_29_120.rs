//! B-2026-09-29-120 -- a caller-retained param's struct field handed to a
//! callee that returns it on some paths only is passed as a clone the callee
//! owns per path.

use super::*;

/// B-2026-09-29-120 -- a nested struct field of a by-value struct param that
/// holds a `shared` value (`q.u` over `W { u: S }`, `S { h: Sh, id: i64 }`),
/// or a `let x = q.u;` view of it, handed to a callee that returns that
/// parameter on ONE path (`fn maybe(s: S, b: bool) -> S { if b { return s }
/// mk(0) }`). B-2026-09-29-101 cloned it only for a callee that returns it on
/// every path, so here the handing path's result was the caller's own field
/// (2 valgrind errors, an abort in `malloc` under `karac run`). The hand-back
/// gate now admits this argument shape, the callee owns the clone per path,
/// and the call site clones. Covers a free function, a view, a `String`
/// beside the `shared` field, a Drop-less struct, a method, a generic
/// function (direct and through a view), a generic method, an all-paths
/// generic method, a param read after the call, and the fresh-temp and
/// local call sites of the same callee, which keep their answer.
#[test]
fn e2e_caller_retained_field_through_conditional_handback_is_cloned() {
    let Some(out) = run_program(
        r#"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mk(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
struct S5 { h: Sh, t: String, id: i64 }
impl Drop for S5 { fn drop(mut ref self) { println(f"dT{self.id}") } }
fn m5(i: i64) -> S5 { return S5 { h: Sh { k: i }, t: f"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa{i}", id: i } }
struct S4 { h: Sh, id: i64 }
fn m4(i: i64) -> S4 { return S4 { h: Sh { k: i }, id: i } }
struct W3 { u: S2, n: i64 }
struct W5 { u: S5, n: i64 }
struct W4 { u: S4, n: i64 }
struct Kp { a: i64 }
impl Kp {
  fn mk4(ref self, s: S4, b: bool) -> S4 { if b { return s } m4(0) }
  fn gm[T](ref self, s: T, b: bool, w: T) -> T { if b { return s } w }
  fn gk[T](ref self, s: T) -> T { return s }
}
fn maybe(s: S2, b: bool) -> S2 { if b { return s } mk(0) }
fn maybe5(s: S5, b: bool) -> S5 { if b { return s } m5(0) }
fn maybe4(s: S4, b: bool) -> S4 { if b { return s } m4(0) }
fn gmaybe[T](s: T, b: bool, w: T) -> T { if b { return s } w }
fn f1(q: W3, b: bool) -> S2 { return maybe(q.u, b) }
fn f2(q: W3, b: bool) -> S2 { let x = q.u; return maybe(x, b) }
fn f3(q: W5, b: bool) -> S5 { return maybe5(q.u, b) }
fn f4(q: W4, b: bool) -> S4 { return maybe4(q.u, b) }
fn f5(q: W4, b: bool) -> S4 { let kp = Kp { a: 1 }; return kp.mk4(q.u, b) }
fn f6(q: W3, b: bool) -> S2 { return gmaybe(q.u, b, mk(0)) }
fn f7(q: W3, b: bool) -> S2 { let x = q.u; return gmaybe(x, b, mk(0)) }
fn f8(q: W4, b: bool) -> S4 { let kp = Kp { a: 1 }; return kp.gm(q.u, b, m4(0)) }
fn f9(q: W4) -> S4 { let kp = Kp { a: 1 }; return kp.gk(q.u) }
fn f10(q: W3, b: bool) -> i64 { let r = maybe(q.u, b); println(f"n{q.n} r{r.id} k{r.h.k}"); return r.id * 10 + q.n }
fn main() {
  println("-c1"); let a = f1(W3 { u: mk(9), n: 2 }, true); println(f"r{a.id}{a.h.k}"); let a2 = f1(W3 { u: mk(8), n: 2 }, false); println(f"r{a2.id}{a2.h.k}")
  println("-c2"); let b = f2(W3 { u: mk(7), n: 2 }, true); println(f"r{b.id}{b.h.k}"); let b2 = f2(W3 { u: mk(6), n: 2 }, false); println(f"r{b2.id}{b2.h.k}")
  println("-c3"); let c = f3(W5 { u: m5(5), n: 2 }, true); println(f"r{c.id}{c.h.k}{c.t.len()}"); let c2 = f3(W5 { u: m5(4), n: 2 }, false); println(f"r{c2.id}{c2.h.k}{c2.t.len()}")
  println("-c4"); let d = f4(W4 { u: m4(3), n: 2 }, true); println(f"r{d.id}{d.h.k}"); let d2 = f4(W4 { u: m4(2), n: 2 }, false); println(f"r{d2.id}{d2.h.k}")
  println("-c5"); let e = f5(W4 { u: m4(13), n: 2 }, true); println(f"r{e.id}{e.h.k}"); let e2 = f5(W4 { u: m4(12), n: 2 }, false); println(f"r{e2.id}{e2.h.k}")
  println("-c6"); let g = f6(W3 { u: mk(19), n: 2 }, true); println(f"r{g.id}{g.h.k}"); let g2 = f6(W3 { u: mk(18), n: 2 }, false); println(f"r{g2.id}{g2.h.k}")
  println("-c7"); let h = f7(W3 { u: mk(17), n: 2 }, true); println(f"r{h.id}{h.h.k}"); let h2 = f7(W3 { u: mk(16), n: 2 }, false); println(f"r{h2.id}{h2.h.k}")
  println("-c8"); let i = f8(W4 { u: m4(15), n: 2 }, true); println(f"r{i.id}{i.h.k}"); let i2 = f8(W4 { u: m4(14), n: 2 }, false); println(f"r{i2.id}{i2.h.k}")
  println("-c9"); let j = f9(W4 { u: m4(11), n: 2 }); println(f"r{j.id}{j.h.k}")
  println("-c10"); let l = f10(W3 { u: mk(21), n: 3 }, true); println(f"s{l}"); let l2 = f10(W3 { u: mk(22), n: 4 }, false); println(f"s{l2}")
  println("-c11"); let z = maybe(mk(31), true); let y = mk(32); let x = maybe(y, false); println(f"z{z.id} x{x.id}")
  println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "-c1\nr99\ndS9\ndS8\nr00\ndS0\n-c2\nr77\ndS7\ndS6\nr00\ndS0\n-c3\nr5531\ndT5\ndT4\nr0031\ndT0\n-c4\nr33\nr00\n-c5\nr1313\nr00\n-c6\ndS0\nr1919\ndS19\ndS18\nr00\ndS0\n-c7\ndS0\nr1717\ndS17\ndS16\nr00\ndS0\n-c8\nr1515\nr00\n-c9\nr1111\n-c10\nn3 r21 k21\ndS21\ns213\ndS22\nn4 r0 k0\ndS0\ns4\n-c11\ndS32\nz31 x0\ndS0\ndS31\nend\n", "got:\n{out}");
}
