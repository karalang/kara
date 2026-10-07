//! B-2026-10-01-52: a by-value param handed back out of a seeded constructor's arm runs its `Drop` bodies once.

use super::*;

/// B-2026-10-01-52 — a by-value param wrapped in a constructor the callee
/// destructures at once (`match Option.Some(a) { Some(v) => v, None => .. }`)
/// and handed back out of the arm is the param under another name, so the
/// caller stands its own cleanup down exactly as for `return a`. It was read
/// as a fresh value: the caller ran the argument's bodies at the call AND over
/// the result (`d1 d2 y2 d1 d2` on every surface for a `Vec[R]`, a double free
/// compiled for an `Array`). Covers a block arm, a tail arm, a named envelope,
/// `if let` followed by an unreachable `return`, a rebind of the arm binding,
/// `Ok` with a dead `Err` arm, and a struct param through a named envelope.
#[test]
fn e2e_param_handed_back_through_seeded_ctor_arm_runs_bodies_once() {
    let Some(out) = run_program(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"aaa" } }
fn h1(a: Vec[R]) -> Vec[R] { match Option.Some(a) { Some(v) => { return v }, None => { return Vec.new() } } }
fn h2(a: Array[R, 2]) -> Array[R, 2] { match Option.Some(a) { Some(v) => { return v }, None => { return [mkr(90), mkr(91)] } } }
fn h3(a: Vec[R]) -> Vec[R] { let o = Option.Some(a); match o { Some(v) => { return v }, None => { return Vec.new() } } }
fn h4(a: Vec[R]) -> Vec[R] { match Option.Some(a) { Some(v) => v, None => Vec.new() } }
fn h5(a: Vec[R]) -> Vec[R] { if let Option.Some(v) = Option.Some(a) { return v } return Vec.new() }
fn h6(a: Vec[R]) -> Vec[R] { match Option.Some(a) { Some(v) => { let u = v; println("r1"); return u }, None => { return Vec.new() } } }
fn h7(a: Vec[R]) -> Vec[R] { let r: Result[Vec[R], i64] = Ok(a); match r { Ok(v) => v, Err(_) => Vec.new() } }
fn h8(a: R) -> R { let o = Some(a); match o { Some(v) => v, None => mkr(99) } }
fn main() {
  println("-h1"); let a1: Vec[R] = [mkr(1), mkr(2)]; let z1 = h1(a1); println(f"y{z1.len()}");
  println("-h2"); let a2: Array[R, 2] = [mkr(3), mkr(4)]; let z2 = h2(a2); println(f"y{z2[0].id}");
  println("-h3"); let a3: Vec[R] = [mkr(5), mkr(6)]; let z3 = h3(a3); println(f"y{z3.len()}");
  println("-h4"); let a4: Vec[R] = [mkr(7), mkr(8)]; let z4 = h4(a4); println(f"y{z4.len()}");
  println("-h5"); let a5: Vec[R] = [mkr(9), mkr(10)]; let z5 = h5(a5); println(f"y{z5.len()}");
  println("-h6"); let a6: Vec[R] = [mkr(11), mkr(12)]; let z6 = h6(a6); println(f"y{z6.len()}");
  println("-h7"); let a7: Vec[R] = [mkr(13), mkr(14)]; let z7 = h7(a7); println(f"y{z7.len()}");
  println("-h8"); let z8 = h8(mkr(15)); println(f"y{z8.id}")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "-h1\ny2\nd1\nd2\n-h2\ny3\nd3\nd4\n-h3\ny2\nd5\nd6\n-h4\ny2\nd7\nd8\n-h5\ny2\nd9\nd10\n-h6\nr1\ny2\nd11\nd12\n-h7\ny2\nd13\nd14\n-h8\ny15\nd15\n", "got:\n{out}");
}
