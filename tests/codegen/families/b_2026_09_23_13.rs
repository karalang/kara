//! B-2026-09-23-13: a by-value `Vec` param rebound out of a seeded `Option` / `Result` arm runs its element bodies once.

use super::*;

/// B-2026-09-23-13 — a by-value `Vec[R]` param seeded into an `Option` /
/// `Result` (`match Option.Some(a) { Some(v) => { let u = v; .. } }`) and
/// rebound out of the arm runs each element's `Drop` body once, in the caller.
/// The arm binding was not known to be a view of the caller's `Vec`, so the
/// rebind armed an element-bodies walker and the caller ran the bodies again
/// (`d3 d4 r1 d3 d4` on every compiled surface). Covers `match`, `if let`,
/// `let .. else`, a named envelope, `Ok` and `Err`, a conditional rebind, a
/// chain, an onward by-value call and a loop, with a non-rebinding arm and
/// two local envelopes as guards.
#[test]
fn e2e_seeded_vec_param_arm_rebind_runs_bodies_once() {
    let Some(out) = run_program(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"aaa" } }
fn mkv(i: i64) -> Vec[R] { return [mkr(i), mkr(i + 1)] }
fn take(v: Vec[R]) -> i64 { println(f"t{v.len()}"); return 1 }
fn f1(a: Vec[R]) -> i64 { match Option.Some(a) { Some(v) => { println(f"r{v.len()}"); return 7 }, None => { return 0 } } }
fn f2(a: Vec[R]) -> i64 { match Option.Some(a) { Some(v) => { let u = v; println("r1"); return 7 }, None => { return 0 } } }
fn f3(a: Vec[R]) -> i64 { if let Some(v) = Option.Some(a) { let u = v; println("r1"); return 7 } return 0 }
fn f4(a: Vec[R]) -> i64 { let o = Option.Some(a); match o { Some(v) => { let u = v; println("r1"); return 7 }, None => { return 0 } } }
fn f6(a: Vec[R]) -> i64 { match Result.Ok(a) { Ok(v) => { let u = v; println("r1"); return 7 }, Err(e) => { return 0 } } }
fn h2(a: Vec[R]) -> i64 { match Option.Some(a) { Some(v) => { let u = v; return take(u) }, None => { return 0 } } }
fn h3(a: Vec[R]) -> i64 { let Some(v) = Option.Some(a) else { return 0 }; let u = v; println("r1"); return 7 }
fn h4(a: Vec[R]) -> i64 { match Result.Err(a) { Ok(x) => { return 1 }, Err(v) => { let u = v; println("r1"); return 7 } } }
fn h6(a: Vec[R], c: bool) -> i64 { match Option.Some(a) { Some(v) => { if c { let u = v; println("r1"); } return 7 }, None => { return 0 } } }
fn h7(a: Vec[R]) -> i64 { match Option.Some(a) { Some(v) => { let u = v; let w = u; println("r1"); return 7 }, None => { return 0 } } }
fn l1() -> i64 { match Option.Some(mkv(40)) { Some(v) => { let u = v; println("r1"); return 7 }, None => { return 0 } } }
fn l3() -> i64 { let a = mkv(42); match Option.Some(a) { Some(v) => { let u = v; println("r1"); return 7 }, None => { return 0 } } }
fn main() {
  println("-f1"); let a1: Vec[R] = [mkr(1), mkr(2)]; let z1 = f1(a1); println(f"y{z1}")
  println("-f2"); let a2: Vec[R] = [mkr(3), mkr(4)]; let z2 = f2(a2); println(f"y{z2}")
  println("-f3"); let a3: Vec[R] = [mkr(5), mkr(6)]; let z3 = f3(a3); println(f"y{z3}")
  println("-f4"); let a4: Vec[R] = [mkr(7), mkr(8)]; let z4 = f4(a4); println(f"y{z4}")
  println("-f6"); let a6: Vec[R] = [mkr(11), mkr(12)]; let z6 = f6(a6); println(f"y{z6}")
  println("-h2"); let b2: Vec[R] = [mkr(13), mkr(14)]; let y2 = h2(b2); println(f"y{y2}")
  println("-h3"); let b3: Vec[R] = [mkr(15), mkr(16)]; let y3 = h3(b3); println(f"y{y3}")
  println("-h4"); let b4: Vec[R] = [mkr(17), mkr(18)]; let y4 = h4(b4); println(f"y{y4}")
  println("-h6"); let b6: Vec[R] = [mkr(19), mkr(20)]; let y6 = h6(b6, true); println(f"y{y6}")
  println("-h6f"); let b7: Vec[R] = [mkr(21), mkr(22)]; let y7 = h6(b7, false); println(f"y{y7}")
  println("-h7"); let b8: Vec[R] = [mkr(23), mkr(24)]; let y8 = h7(b8); println(f"y{y8}")
  println("-l1"); let w1 = l1(); println(f"y{w1}")
  println("-l3"); let w3 = l3(); println(f"y{w3}")
  println("-lp"); for i in 0..2 { let c: Vec[R] = [mkr(60 + i)]; let q = f2(c); println(f"y{q}") }
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "-f1\nr2\nd1\nd2\ny7\n-f2\nr1\nd3\nd4\ny7\n-f3\nr1\nd5\nd6\ny7\n-f4\nr1\nd7\nd8\ny7\n-f6\nr1\nd11\nd12\ny7\n-h2\nt2\nd13\nd14\ny1\n-h3\nr1\nd15\nd16\ny7\n-h4\nr1\nd17\nd18\ny7\n-h6\nr1\nd19\nd20\ny7\n-h6f\nd21\nd22\ny7\n-h7\nr1\nd23\nd24\ny7\n-l1\nd40\nd41\nr1\ny7\n-l3\nd42\nd43\nr1\ny7\n-lp\nr1\nd60\ny7\nr1\nd61\ny7\n", "got:\n{out}");
}
