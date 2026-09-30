//! B-2026-09-30-6: a `let mut` local handed off in a branch that may not run, then reassigned.

use super::*;

/// B-2026-09-30-6 — a `let mut` local whose container walk runs its payload's
/// `Drop` body, moved, kept by a call or consumed as an owned-`self` receiver
/// inside a branch that may not run and then reassigned, runs that body once
/// on every path. The hand-off retracted the walk on every path, so at
/// `c = false` the payload was freed with no body run; and a reassignment in
/// the same branch inside a loop ran the consumed payload's body twice. The
/// reassignment's displaced-payload walk now consults the per-path flag and
/// re-arms it. Covers a `let` move, a keeping call, a reassignment in the
/// branch of a loop, a receiver, an `Option` local, two reassignments, and a
/// reassignment on the other branch, each at both values of `c`.
#[test]
fn interp_let_mut_local_handed_off_in_an_untaken_branch_then_reassigned_runs_its_body_once() {
    let out = run(r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"heap-string-longer-than-sso-{i}" } }
enum E { A(S), B(i64) }
impl E {
  fn m1(self) -> i64 { return match self { E.A(s) => s.id, E.B(n) => n } }
}
fn keep(e: E) -> E { return e }
fn r1(c: bool) -> i64 { let mut t = E.A(mks(1)); if c { let y = t; println("y") }; t = E.A(mks(2)); println("re"); return 0 }
fn r2(c: bool) -> i64 { let mut t = E.A(mks(3)); if c { let k = keep(t); println("k") }; t = E.A(mks(4)); println("re"); return t.m1() }
fn r3(c: bool) -> i64 { let mut t = E.A(mks(5)); let mut i = 0; while i < 2 { if c { let y = t; println("y"); t = E.A(mks(6 + i)); }; i = i + 1; }; return 0 }
fn r5(c: bool) -> i64 { let mut t = E.A(mks(20)); if c { println(f"m {t.m1()}") }; t = E.A(mks(21)); println("re"); return 0 }
fn r6(c: bool) -> i64 { let mut o = Option.Some(mks(30)); if c { let y = o; println("y") }; o = Option.Some(mks(31)); println("re"); return 0 }
fn r7(c: bool) -> i64 { let mut t = E.A(mks(40)); if c { let y = t; println("y") }; t = E.A(mks(41)); t = E.A(mks(42)); println("re"); return 0 }
fn r8(c: bool) -> i64 { let mut t = E.A(mks(50)); if c { let y = t; println("y") }; if not c { t = E.A(mks(51)); }; println("re"); return 0 }
fn main() {
  let mut k = 0;
  while k < 2 { let c = k == 1; println(f"c {c}")
    println(f"r {r1(c)}"); println(f"r {r2(c)}"); println(f"r {r3(c)}");
    println(f"r {r5(c)}"); println(f"r {r6(c)}"); println(f"r {r7(c)}"); println(f"r {r8(c)}")
    k = k + 1; }
  println("end")
}
"#);
    assert_eq!(out, "c false\ndS1\ndS2\nre\nr 0\ndS3\nre\ndS4\nr 4\ndS5\nr 0\ndS20\ndS21\nre\nr 0\ndS30\ndS31\nre\nr 0\ndS40\ndS41\ndS42\nre\nr 0\ndS50\ndS51\nre\nr 0\nc true\ndS1\ny\ndS2\nre\nr 0\ndS3\nk\nre\ndS4\nr 4\ndS5\ny\ndS6\ny\ndS7\nr 0\ndS20\nm 20\ndS21\nre\nr 0\ndS30\ny\ndS31\nre\nr 0\ndS40\ny\ndS41\ndS42\nre\nr 0\ndS50\ny\nre\nr 0\nend\n");
}
