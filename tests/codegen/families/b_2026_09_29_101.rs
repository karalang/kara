//! B-2026-09-29-101 -- a caller-retained param's struct field handed to a
//! callee that always returns it is passed as a clone.

use super::*;

/// B-2026-09-29-101 -- a nested struct field of a by-value struct param that
/// holds a `shared` value (`q.u` over `W { u: S }`, `S { h: Sh, id: i64 }`),
/// or a `let x = q.u;` view of it, handed to a callee that returns that
/// parameter on every path (`fn keep(s: S) -> S { s }`). The param is
/// caller-retained, so the caller frees its argument whole; the callee handed
/// the field's own words back, and the result read freed memory (a garbage
/// `h.k`, an abort in `malloc` at -O0). Covers a free function, a rebinding
/// one, a generic one, a method, a `String` beside the `shared` field, a
/// result bound and dropped inside the callee, and a callee that never hands
/// the field back (`eat`), which must keep receiving the view.
#[test]
fn e2e_caller_retained_field_through_handback_callee_is_cloned() {
    let Some(out) = run_program(
        r#"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mk(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
struct S5 { h: Sh, t: String, id: i64 }
impl Drop for S5 { fn drop(mut ref self) { println(f"dT{self.id}") } }
struct W3 { u: S2, n: i64 }
struct S4 { h: Sh, id: i64 }
struct W6 { u: S4, n: i64 }
struct Kp { a: i64 }
impl Kp { fn keep4(ref self, s: S4) -> S4 { return s } }
struct W7 { u: S5, n: i64 }
fn keep(s: S2) -> S2 { return s }
fn keepr(s: S2) -> S2 { let m = s; return m }
fn keep5(s: S5) -> S5 { return s }
fn keep2[T](x: T) -> T { return x }
fn eat(s: S2) { println(f"e{s.id}") }
fn f1(q: W3) -> S2 { return keep(q.u) }
fn f2(q: W3) -> S2 { let k = keep(q.u); return k }
fn f3(q: W3) -> S2 { let x = q.u; return keep(x) }
fn f4(q: W3) -> S2 { return keepr(q.u) }
fn f5(q: W3) -> S2 { let x = q.u; let W3 { n, .. } = q; println(f"n{n}"); return keep(x) }
fn f6(q: W3) -> S2 { return keep2(q.u) }
fn f7(q: W3) -> S2 { let x = q.u; return keep2(x) }
fn f8(q: W7) -> S5 { return keep5(q.u) }
fn f9(q: W3) { let k = keep(q.u); println(f"k{k.id}") }
fn f10(q: W3) { eat(q.u) }
fn f11(q: W6) -> S4 { let kp = Kp { a: 1 }; return kp.keep4(q.u) }
fn f12(q: W6) -> S4 { let kp = Kp { a: 1 }; let x = q.u; return kp.keep4(x) }
fn main() {
  println("-c1"); let a = f1(W3 { u: mk(9), n: 2 }); println(f"r{a.id}{a.h.k}");
  println("-c2"); let b = f2(W3 { u: mk(8), n: 2 }); println(f"r{b.id}{b.h.k}");
  println("-c3"); let c = f3(W3 { u: mk(7), n: 2 }); println(f"r{c.id}{c.h.k}");
  println("-c4"); let d = f4(W3 { u: mk(6), n: 2 }); println(f"r{d.id}{d.h.k}");
  println("-c5"); let e = f5(W3 { u: mk(5), n: 3 }); println(f"r{e.id}{e.h.k}");
  println("-c6"); let g = f6(W3 { u: mk(4), n: 2 }); println(f"r{g.id}{g.h.k}");
  println("-c7"); let h = f7(W3 { u: mk(3), n: 2 }); println(f"r{h.id}{h.h.k}");
  println("-c8"); let i = f8(W7 { u: S5 { h: Sh { k: 2 }, t: f"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa{1}", id: 2 }, n: 2 }); println(f"r{i.id}{i.h.k}{i.t.len()}");
  println("-c9"); f9(W3 { u: mk(1), n: 2 });
  println("-c10"); f10(W3 { u: mk(0), n: 2 });
  println("-c11"); let j = f11(W6 { u: S4 { h: Sh { k: 7 }, id: 1 }, n: 2 }); println(f"r{j.id}{j.h.k}");
  println("-c12"); let l = f12(W6 { u: S4 { h: Sh { k: 8 }, id: 2 }, n: 2 }); println(f"r{l.id}{l.h.k}");
  println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "-c1\nr99\ndS9\n-c2\nr88\ndS8\n-c3\nr77\ndS7\n-c4\nr66\ndS6\n-c5\nn3\nr55\ndS5\n-c6\nr44\ndS4\n-c7\nr33\ndS3\n-c8\nr2231\ndT2\n-c9\nk1\ndS1\n-c10\ne0\ndS0\n-c11\nr17\n-c12\nr28\nend\n", "got:\n{out}");
}
