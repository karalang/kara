//! B-2026-09-29-75: a by-value enum param used as the receiver of an owned-`self` method that takes its payload.

use super::*;

/// B-2026-09-29-75 — a by-value user-enum parameter handed, on every path, as
/// the receiver of an owned-`self` method whose arm takes the payload over runs
/// the payload's `Drop` body once. The method's arm (or, on a path that skips its
/// `match`, B-2026-09-29-77's adoption) runs it, and the caller of the frame that
/// holds the parameter kept its own walk as well, since a by-value param's bodies
/// are the caller's: `fn p2(t: E) -> i64 { let x = t.m2(); .. }` printed
/// `dS1 p2 1 dS1`. Covers a fresh temp and a named local argument, a method
/// argument (`h.q(t)`), a forwarding hop (`fwd` into `p2`), the call inside an
/// operator, an interpolation and a `let .. else`, a payload hand-back, a loop,
/// and a payload-free variant.
#[test]
fn asan_enum_param_as_owned_self_receiver_runs_the_payload_body_once() {
    assert_clean_asan_run(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"heap-string-longer-than-sso-{i}" } }
enum E { A(S), B(i64) }
impl E {
  fn m2(self) -> i64 { match self { E.A(s) => s.id, E.B(n) => n } }
  fn t1(self, c: bool) -> i64 { if c { match self { E.A(s) => s.id, E.B(n) => n } } else { 0 } }
  fn mb(self) -> i64 { let E.A(s) = self else { return 0 }; return s.id }
  fn take(self) -> S { match self { E.A(s) => s, E.B(n) => mks(n) } }
}
struct H { k: i64 }
impl H { fn q(ref self, t: E) -> i64 { return t.m2() + self.k } }
fn p2(t: E) -> i64 { let x = t.m2(); println(f"p2 {x}"); return x }
fn p6(t: E) -> i64 { return t.m2() }
fn p7(t: E, c: bool) -> i64 { let x = t.t1(c); println(f"p7 {x}"); return x }
fn fwd(t: E) -> i64 { return p2(t) }
fn pa(t: E) -> i64 { let x = t.m2() + 1; println(f"pa {x}"); return x }
fn pi(t: E) -> i64 { println(f"pi {t.m2()}"); return 0 }
fn pb(t: E) -> i64 { let x = t.mb(); println(f"pb {x}"); return x }
fn p9(t: E) -> S { return t.take() }
fn main() {
  println(f"r {p2(E.A(mks(1)))}");
  let e = E.A(mks(2)); println(f"r {p2(e)}");
  println(f"r {p6(E.A(mks(3)))}");
  println(f"r {p7(E.A(mks(4)), false)}");
  println(f"r {p7(E.A(mks(5)), true)}");
  println(f"r {fwd(E.A(mks(6)))}");
  let h = H { k: 100 }; println(f"r {h.q(E.A(mks(7)))}");
  let e8 = E.A(mks(8)); println(f"r {h.q(e8)}");
  println(f"r {pa(E.A(mks(9)))}");
  println(f"r {pi(E.A(mks(10)))}");
  println(f"r {pb(E.A(mks(11)))}");
  let s = p9(E.A(mks(12))); println(f"got {s.id}");
  let mut i = 0;
  while i < 2 { let w = E.A(mks(13 + i)); println(f"r {p2(w)}"); i = i + 1; }
  println(f"r {p2(E.B(15))}");
  println("end")
}
"#,
        &[
            "dS1", "p2 1", "r 1", "dS2", "p2 2", "r 2", "dS3", "r 3", "dS4", "p7 0", "r 0", "dS5",
            "p7 5", "r 5", "dS6", "p2 6", "r 6", "dS7", "r 107", "dS8", "r 108", "dS9", "pa 10",
            "r 10", "dS10", "pi 10", "r 0", "dS11", "pb 11", "r 11", "got 12", "dS12", "dS13",
            "p2 13", "r 13", "dS14", "p2 14", "r 14", "p2 15", "r 15", "end",
        ],
        "asan_enum_param_as_owned_self_receiver_runs_the_payload_body_once",
    );
}
