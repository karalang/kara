//! B-2026-09-30-19: a match / if-let arm binding that shadows a live local.

use super::*;

/// B-2026-09-30-19 — a `match` or `if let` arm binding that SHADOWS a local
/// still in scope, moved inside the arm, no longer takes the outer local's
/// `Drop` body with it. Both backends keyed the move by NAME: the interpreter
/// left the arm's move record on `t` after the arm, and codegen's per-path bit
/// `cmflag.t` was cleared by the arm's move and then guarded the outer `t`'s
/// scope-exit drop. `let t = mk(50); { match h { Hr.P(t) => { let u = t } .. } };
/// if c { let w = t }` ran no `dS50` at `c = false` on every surface, the
/// interpreter included, and leaked its heap. The shadowed name's records are
/// now saved when the arm binds and put back when it ends. Covers the reported
/// shape, `if let`, a conditional move in the arm, an early return after and
/// before the arm's move, a loop, a doubly nested shadow, and an outer local
/// moved on one path before the arm.
#[test]
fn e2e_arm_binding_shadowing_a_live_local_leaves_it_its_body() {
    let Some(out) = run_program(
        r#"struct S1 { v: i64, s: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS{self.v}") } }
enum Hr { P(S1), Q }
fn mk(v: i64) -> S1 { return S1 { v: v, s: f"h{v}" } }
fn take(s: S1) -> i64 { return s.v }
fn c1(c: bool, d: bool) -> i64 {
  let t = mk(1);
  match Hr.P(mk(2)) { Hr.P(t) => { if d { let u = t; println(f"u{u.v}") } } Hr.Q => {} }
  if c { let w = t; println(f"w{w.v}") }
  return 0
}
fn c2(c: bool) -> i64 {
  let t = mk(3);
  match Hr.P(mk(4)) { Hr.P(t) => { let u = t; if c { return u.v } println(f"u{u.v}") } Hr.Q => {} }
  println(f"t{t.v}")
  return 0
}
fn c3(c: bool) -> i64 {
  let t = mk(5);
  match Hr.P(mk(6)) { Hr.P(t) => { if c { return 9 } let u = t; println(f"u{u.v}") } Hr.Q => {} }
  println(f"t{t.v}")
  return 0
}
fn c4(c: bool) -> i64 {
  let mut i = 0;
  while i < 2 {
    let t = mk(10 + i);
    if let Hr.P(t) = Hr.P(mk(20 + i)) { let u = t; println(f"u{u.v}") }
    if c { let w = t; println(f"w{w.v}") }
    i = i + 1;
  };
  return 0
}
fn c5(c: bool) -> i64 {
  let t = mk(30);
  match Hr.P(mk(31)) { Hr.P(t) => { match Hr.P(mk(32)) { Hr.P(t) => { take(t); } Hr.Q => {} } println(f"m{t.v}") } Hr.Q => {} }
  if c { take(t); }
  return 0
}
fn c6(c: bool) -> i64 {
  let t = mk(40);
  if c { take(t); }
  match Hr.P(mk(41)) { Hr.P(t) => { take(t); } Hr.Q => {} }
  println("end6")
  return 0
}
fn f3(c: bool) -> i64 {
  let t = mk(50);
  { let h = Hr.P(mk(51)); match h { Hr.P(t) => { let u = t; println(f"n{u.v}") } Hr.Q => {} } }
  if c { let w = t; println(f"w{w.v}") }
  return 0
}
fn a2() -> i64 {
  let t = mk(60);
  if let Hr.P(t) = Hr.P(mk(61)) { let u = t; println(f"u{u.v}") }
  println(f"t{t.v}")
  return 0
}
fn main() {
  println(f"r {f3(false)}"); println(f"r {f3(true)}"); println(f"r {a2()}")
  println(f"r {c1(false, false)}"); println(f"r {c1(false, true)}"); println(f"r {c1(true, false)}"); println(f"r {c1(true, true)}")
  println(f"r {c2(false)}"); println(f"r {c2(true)}")
  println(f"r {c3(false)}"); println(f"r {c3(true)}")
  println(f"r {c4(false)}"); println(f"r {c4(true)}")
  println(f"r {c5(false)}"); println(f"r {c5(true)}")
  println(f"r {c6(false)}"); println(f"r {c6(true)}")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "n51\ndS51\ndS50\nr 0\nn51\ndS51\nw50\ndS50\nr 0\nu61\ndS61\nt60\ndS60\nr 0\ndS2\ndS1\nr 0\nu2\ndS2\ndS1\nr 0\ndS2\nw1\ndS1\nr 0\nu2\ndS2\nw1\ndS1\nr 0\nu4\ndS4\nt3\ndS3\nr 0\ndS4\ndS3\nr 4\nu6\ndS6\nt5\ndS5\nr 0\ndS6\ndS5\nr 9\nu20\ndS20\ndS10\nu21\ndS21\ndS11\nr 0\nu20\ndS20\nw10\ndS10\nu21\ndS21\nw11\ndS11\nr 0\ndS32\nm31\ndS31\ndS30\nr 0\ndS32\nm31\ndS31\ndS30\nr 0\ndS40\ndS41\nend6\nr 0\ndS40\ndS41\nend6\nr 0\n", "got:\n{out}");
}
