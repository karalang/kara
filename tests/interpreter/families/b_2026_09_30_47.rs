//! B-2026-09-30-47: a payload-binding match arm taking some fields of its variant inside a branch.

use super::*;

/// B-2026-09-30-47 — a payload-binding `match` arm that takes only SOME
/// fields of its variant, on a `let` local, inside a branch that may not run,
/// masked the local's payload walk on every path, so the path that never ran
/// the arm lost the taken field's `Drop` body: `let t = F.P(mks(10),
/// mks(11)); if c { match t { F.P(x, _) => { let k = x; .. } .. } }` printed
/// only `dS11` at `c = false`. Such an arm now clears the local's per-path
/// bit, and the walk the local runs on a path whose bit is clear is masked by
/// what the per-path arms took. Covers arms of two variants, a top-level
/// match, a doubly nested branch with an `else`, an early return, an arm that
/// binds both fields but moves one, and a conditional move inside the arm.
#[test]
fn interp_partial_payload_arm_in_a_branch_leaves_the_untaken_path_every_body() {
    let out = run(r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"s{i}" } }
enum F { P(S, S), Q(S, S), R(i64) }
fn h1(c: bool) -> i64 {
  let t = F.P(mks(10), mks(11));
  if c { match t { F.P(x, _) => { let k = x; println(f"k{k.id}") } F.Q(_, y) => { let k = y; println("q") } F.R(n) => {} } }
  return 0
}
fn h2(c: bool) -> i64 {
  let t = F.Q(mks(20), mks(21));
  if c { match t { F.P(x, _) => { let k = x; println(f"k{k.id}") } F.Q(_, y) => { let k = y; println(f"q{k.id}") } F.R(n) => {} } }
  return 0
}
fn h3(c: bool) -> i64 {
  let t = F.P(mks(30), mks(31));
  match t { F.P(x, _) => { let k = x; println(f"k{k.id}") } F.Q(_, y) => {} F.R(n) => {} }
  return 0
}
fn h4(c: bool, d: bool) -> i64 {
  let t = F.P(mks(40), mks(41));
  if c { if d { match t { F.P(_, y) => { let k = y; println(f"k{k.id}") } _ => {} } } else { println("nd") } }
  return 0
}
fn h5(c: bool) -> i64 {
  let t = F.P(mks(50), mks(51));
  if c { match t { F.P(x, _) => { let k = x; return k.id } _ => {} } }
  return 0
}
fn h6(c: bool) -> i64 {
  let t = F.P(mks(60), mks(61));
  if c { match t { F.P(x, y) => { let k = x; println(f"k{k.id}") } _ => {} } }
  return 0
}
fn h7(c: bool) -> i64 {
  let t = F.P(mks(70), mks(71));
  if c { match t { F.P(x, _) => { if x.id > 100 { let k = x; println("big") } } _ => {} } }
  return 0
}
fn main() {
  println(f"r {h1(false)}"); println(f"r {h1(true)}");
  println(f"r {h2(false)}"); println(f"r {h2(true)}");
  println(f"r {h3(false)}");
  println(f"r {h4(false, false)}"); println(f"r {h4(true, false)}"); println(f"r {h4(true, true)}");
  println(f"r {h5(false)}"); println(f"r {h5(true)}");
  println(f"r {h6(false)}"); println(f"r {h6(true)}");
  println(f"r {h7(false)}"); println(f"r {h7(true)}")
}
"#);
    assert_eq!(out, "dS11\ndS10\nr 0\nk10\ndS10\ndS11\nr 0\ndS21\ndS20\nr 0\nq21\ndS21\ndS20\nr 0\nk30\ndS30\ndS31\nr 0\ndS41\ndS40\nr 0\nnd\ndS41\ndS40\nr 0\nk41\ndS41\ndS40\nr 0\ndS51\ndS50\nr 0\ndS50\ndS51\nr 50\ndS61\ndS60\nr 0\nk60\ndS60\ndS61\nr 0\ndS71\ndS70\nr 0\ndS70\ndS71\nr 0\n");
}
