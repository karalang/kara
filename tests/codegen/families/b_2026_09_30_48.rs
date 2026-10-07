//! B-2026-09-30-48: a boxed Option/Result payload moved on inside a branch that may not run.

use super::*;

/// B-2026-09-30-48 — an `Option`/`Result` local whose payload is HEAP-BOXED,
/// matched by an arm that moves the payload binding on (`let k = s`) inside a
/// branch that may not run, took the box's interior free off every path: the
/// view-move retraction cleared the box action's inner drop at compile time,
/// so `let t: Option[S] = Some(mks(7)); if c { match t { Some(s) => { let k =
/// s; .. } .. } }` at `c = false` ran `dS7` but leaked the payload's `String`.
/// The retraction now clears a per-path bit guarding the inner drop when the
/// box action lives in a scope the arm is nested in. Covers `match` and
/// `if let`, an `else` arm, a doubly nested branch, `Result`, a top-level
/// match, an early return out of the arm, and a two-`String` payload.
#[test]
fn e2e_boxed_optres_payload_moved_in_an_untaken_branch_keeps_its_interior_free() {
    let Some(out) = run_program(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
struct W { id: i64, s: String, t: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"s{i}" } }
fn mkw(i: i64) -> W { return W { id: i, s: f"s{i}", t: f"t{i}" } }
fn g1(c: bool) -> i64 {
  let t: Option[S] = Some(mks(1));
  if c { match t { Some(s) => { let k = s; println(f"k{k.id}") } None => {} } }
  return 0
}
fn g2(c: bool) -> i64 {
  let t: Option[S] = Some(mks(2));
  if c { if let Some(s) = t { let k = s; println("k") } } else { println("e") }
  return 0
}
fn g3(c: bool, d: bool) -> i64 {
  let t: Option[S] = Some(mks(3));
  if c { if d { match t { Some(s) => { let k = s; println("k") } None => {} } } }
  return 0
}
fn g4(c: bool) -> i64 {
  let t: Result[W, i64] = Ok(mkw(4));
  if c { match t { Ok(w) => { let k = w; println("k") } Err(e) => {} } }
  return 0
}
fn g5(c: bool) -> i64 {
  let t: Option[S] = Some(mks(5));
  match t { Some(s) => { let k = s; println("k") } None => {} }
  return 0
}
fn g6(c: bool) -> i64 {
  let t: Option[S] = Some(mks(6));
  if c { match t { Some(s) => { let k = s; return k.id } None => {} } }
  return 0
}
fn g7(c: bool) -> i64 {
  let t: Option[W] = Some(mkw(7));
  if c { match t { Some(w) => { let k = w; println("k") } None => {} } }
  return 0
}
fn main() {
  println(f"r {g1(false)}"); println(f"r {g1(true)}");
  println(f"r {g2(false)}"); println(f"r {g2(true)}");
  println(f"r {g3(false, true)}"); println(f"r {g3(true, false)}"); println(f"r {g3(true, true)}");
  println(f"r {g4(false)}"); println(f"r {g4(true)}");
  println(f"r {g5(false)}");
  println(f"r {g6(false)}"); println(f"r {g6(true)}");
  println(f"r {g7(false)}"); println(f"r {g7(true)}")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "dS1\nr 0\nk1\ndS1\nr 0\ne\ndS2\nr 0\ndS2\nk\nr 0\ndS3\nr 0\ndS3\nr 0\ndS3\nk\nr 0\ndW4\nr 0\ndW4\nk\nr 0\ndS5\nk\nr 0\ndS6\nr 0\ndS6\nr 6\ndW7\nr 0\ndW7\nk\nr 0\n", "got:\n{out}");
}
