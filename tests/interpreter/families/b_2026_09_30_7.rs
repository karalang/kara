//! B-2026-09-30-7 -- a struct owning a `shared` field, consumed twice by a
//! callee that hands it back, is passed as its use-after-move copy.

use super::*;

/// B-2026-09-30-7 -- `let a = keep(s); let d = keep(s)` over `fn keep(s: S2)
/// -> S2 { return s }` and `S2 { h: Sh, id: i64 }` is a use after move, which
/// the checker warns about and answers with a defensive copy. `S2` is
/// caller-retained (a bare `shared` field declines the callee's entry copy),
/// so the callee handed the caller's own words back and `a` and `d` shared
/// one `Sh` box (2 valgrind errors); through a param's field, `keep(q.u);
/// keep(q.u)`, the first move-out also nulled the field and the second read
/// segfaulted. The first consume now passes the defensive copy, the source
/// keeps its memory and gives its `Drop` body to the value the callee took,
/// and the field is left in place. Covers a local, a param's field and a view
/// of one, a conditional hand-back, a Drop-less struct through methods, a
/// generic callee, a `String` beside the `shared` field, and a consume
/// followed only by a read (one body, as the interpreter counts it).
#[test]
fn interp_caller_retained_struct_moved_twice_into_handback_callee_is_copied() {
    let out = run(r#"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mk(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
struct W3 { u: S2, n: i64 }
struct S4 { h: Sh, id: i64 }
fn m4(i: i64) -> S4 { return S4 { h: Sh { k: i }, id: i } }
struct S5 { h: Sh, t: String, id: i64 }
impl Drop for S5 { fn drop(mut ref self) { println(f"dT{self.id}") } }
struct Kp { z: i64 }
impl Kp {
  fn k4(ref self, s: S4) -> S4 { return s }
  fn kc(ref self, s: S4, b: bool) -> S4 { if b { return s } m4(0) }
}
fn keep(s: S2) -> S2 { return s }
fn keep5(s: S5) -> S5 { return s }
fn keep2[T](x: T) -> T { return x }
fn maybe(s: S2, b: bool) -> S2 { if b { return s } mk(0) }
fn c1() { let s = mk(1); let a = keep(s); let d = keep(s); println(f"{a.id}{a.h.k} {d.id}{d.h.k}") }
fn c2(q: W3) -> i64 { let a = keep(q.u); let d = keep(q.u); return a.id * 100 + d.h.k }
fn c3() { let s = mk(3); let a = maybe(s, true); let d = maybe(s, false); println(f"{a.id}{a.h.k} {d.id}{d.h.k}") }
fn c4(q: W3) -> i64 { let a = maybe(q.u, true); let c = maybe(q.u, false); let d = maybe(q.u, true); return a.id * 100 + c.id * 10 + d.h.k }
fn c5() { let kp = Kp { z: 1 }; let s = m4(5); let a = kp.k4(s); let d = kp.k4(s); let t = m4(6); let b = kp.kc(t, false); let c = kp.kc(t, true); println(f"{a.h.k}{d.h.k} {b.h.k}{c.h.k}") }
fn c6() { let s = mk(6); let a = keep2(s); let d = keep2(s); println(f"{a.h.k}{d.h.k}") }
fn c7() { let s = mk(7); let a = keep(s); println(f"{s.h.k} {a.h.k}") }
fn c8() { let s = S5 { h: Sh { k: 8 }, t: f"aaaaaaaaaaaaaaaaaaaaaaaaaaaaa{4}", id: 8 }; let a = keep5(s); let d = keep5(s); println(f"{a.t.len()}{a.h.k} {d.t.len()}{d.h.k}") }
fn c9(q: W3) -> i64 { let x = q.u; let a = keep(x); let d = keep(x); return a.h.k * 10 + d.h.k }
fn main() {
  println("-c1"); c1()
  println("-c2"); println(f"s{c2(W3 { u: mk(2), n: 2 })}")
  println("-c3"); c3()
  println("-c4"); println(f"s{c4(W3 { u: mk(4), n: 2 })}")
  println("-c5"); c5()
  println("-c6"); c6()
  println("-c7"); c7()
  println("-c8"); c8()
  println("-c9"); println(f"s{c9(W3 { u: mk(9), n: 2 })}")
  println("end")
}
"#);
    assert_eq!(out, "-c1\n11 11\ndS1\ndS1\n-c2\ndS2\ndS2\ns202\n-c3\ndS3\n33 00\ndS0\ndS3\n-c4\ndS4\ndS4\ndS0\ndS4\ns404\n-c5\n55 06\n-c6\n66\ndS6\ndS6\n-c7\n7 7\ndS7\n-c8\n308 308\ndT8\ndT8\n-c9\ndS9\ndS9\ns99\nend\n");
}
