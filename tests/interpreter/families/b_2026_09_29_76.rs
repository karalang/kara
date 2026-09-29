//! B-2026-09-29-76: a let shadowing an enum binding with its own owned-self method result.

use super::*;

/// B-2026-09-29-76 — a `let` that shadows a user-enum binding with the result of
/// an owned-`self` method called on it (`let t = t.m1()`) runs the payload's
/// `Drop` body once. The interpreter froze the shadowed binding's drop slot
/// before the call had marked it moved, then the fresh bind cleared the moved
/// mark, so the slot ran the payload body a second time at scope exit:
/// `let t = E.A(mks(6)); if c { let t = t.m1(); return t }` printed `dS6 r 6 dS6`.
/// Covers the shadow inside a branch, a block, a loop and at function scope, a
/// shadowed by-value parameter, a re-shadow of the result, a shadow by a fresh
/// payload-free value built from the call, and a payload-free receiver. The
/// compiled backend already ran each body once; its cells pin the agreement.
#[test]
fn interp_let_shadow_of_owned_self_receiver_runs_the_payload_body_once() {
    let out = run(r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"heap-string-longer-than-sso-{i}" } }
enum E { A(S), B(i64) }
impl E {
  fn m1(self) -> i64 { return match self { E.A(s) => s.id, E.B(n) => n } }
  fn m4(self) -> i64 { match self { E.A(s) => { return s.id }, E.B(n) => { return n } } }
}
fn v1(c: bool) -> i64 { let t = E.A(mks(6)); if c { let t = t.m1(); return t }; return 0 }
fn v2(c: bool) -> i64 { let t = E.A(mks(7)); if c { let u = t.m1(); return u }; return 0 }
fn v3(c: bool) -> i64 { let t = E.A(mks(8)); if c { let t = t.m4(); return t }; return 0 }
fn v4(c: bool) -> i64 { let t = E.A(mks(9)); if c { let t = t.m4(); println("in") }; return 0 }
fn v5() -> i64 { let t = E.A(mks(10)); let t = t.m1(); return t }
fn v6() -> i64 { let t = E.A(mks(11)); { let t = t.m1(); println(f"blk {t}") }; return 0 }
fn w1(t: E) -> i64 { let t = t.m1(); return t }
fn w2() -> i64 { let t = E.A(mks(21)); let t = t.m1(); let t = t + 1; return t }
fn w3() -> i64 { let mut n = 0; let mut i = 0;
  while i < 2 { let t = E.A(mks(30 + i)); let t = t.m1(); n = n + t; i = i + 1; }
  return n }
fn w4() -> i64 { let t = E.A(mks(40)); let t = E.B(t.m1()); return t.m1() }
fn w5() -> i64 { let t = E.B(5); let t = t.m1(); return t }
fn main() {
  println(f"r {v1(true)}"); println(f"r {v2(true)}"); println(f"r {v3(true)}"); println(f"r {v4(true)}")
  println(f"r {v5()}"); println(f"r {v6()}")
  println(f"r {w1(E.A(mks(20)))}")
  println(f"r {w2()}"); println(f"r {w3()}"); println(f"r {w4()}"); println(f"r {w5()}")
  println("end")
}
"#);
    assert_eq!(out, "dS6\nr 6\ndS7\nr 7\ndS8\nr 8\ndS9\nin\nr 0\ndS10\nr 10\ndS11\nblk 11\nr 0\ndS20\nr 20\ndS21\nr 22\ndS30\ndS31\nr 61\ndS40\nr 40\nr 5\nend\n");
}
