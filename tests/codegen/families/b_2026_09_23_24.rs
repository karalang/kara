//! B-2026-09-23-24: a `let` that takes a by-value param through a branch arm is a view of it on that path

use super::*;

/// B-2026-09-23-24: `let r = if c { a } else { mk() }` over a by-value param `a`, with `r`
/// dying inside the callee, left two owners on the path through the arm: the caller,
/// which runs the param's `Drop` body after the call, and `r`. Every surface ran the
/// body twice, and the `Array` spelling double-freed compiled. On that path `r` is now
/// the param, as `let r = a` is; on the others it owns what its arm minted. Also the
/// rebind half of B-2026-09-23-27 (`let q = r; q`).
#[test]
fn e2e_let_bound_branch_view_of_by_value_param_runs_body_once() {
    let Some(out) = run_program(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct H { r: R, n: i64 }
enum E { A(R), B }
struct K { n: i64 }
impl K { fn m(ref self, a: R, c: bool) -> i64 { let r = if c { a } else { mkr(7) }; println(f"r{r.id}"); 0 } }
fn mkr(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn mka(i: i64) -> Array[R, 2] { return [mkr(i), mkr(i + 1)] }
fn eat(r: R) { println(f"e{r.id}") }
fn f1(a: Array[R, 2], c: bool) -> Array[R, 2] { let r: Array[R, 2] = if c { a } else { mka(8) }; println(f"r{r[0].id}"); mka(30) }
fn f2(a: R, c: bool) -> i64 { let r: R = if c { a } else { mkr(7) }; println(f"r{r.id}"); 0 }
fn f3(a: R, c: i64) -> i64 { let r = match c { 0 => a, 1 => { if true { a } else { mkr(8) } }, _ => mkr(7) }; println(f"r{r.id}"); 0 }
fn f4(a: R, c: bool) -> i64 { let r = { if c { println("t"); a } else { mkr(7) } }; println(f"r{r.id}"); 0 }
fn f5(a: R, b: R, c: bool) -> i64 { let r = if c { a } else { b }; println(f"r{r.id}"); 0 }
fn f6(a: R, c: bool) -> i64 { let r = if c { a } else { mkr(7) }; eat(r); 0 }
fn f7(a: R, c: bool) -> i64 { let r = if c { a } else { mkr(7) }; let q = r; println(f"q{q.id}"); 0 }
fn f8(a: H, c: bool) -> i64 { let r = if c { a } else { H { r: mkr(7), n: 0 } }; println(f"r{r.r.id}"); 0 }
fn f9(a: E, c: bool) -> i64 { let r = if c { a } else { E.B }; match r { E.A(x) => println(f"r{x.id}"), E.B => println("rb") }; 0 }
fn f10(a: Vec[R], c: bool) -> i64 { let r = if c { a } else { [mkr(7)] }; println(f"r{r.len()}"); 0 }
fn f11(a: (R, i64), c: bool) -> i64 { let r = if c { a } else { (mkr(7), 0) }; println(f"r{r.0.id}"); 0 }
fn f12(a: R, o: Option[i64]) -> i64 { let r = if let Some(k) = o { a } else { mkr(7) }; println(f"r{r.id}"); 0 }
fn f13(a: Array[R, 2], c: i64) -> i64 { let r: Array[R, 2] = match c { 0 => a, _ => mka(20) }; println(f"r{r[0].id}"); 0 }
fn f14(a: Array[R, 2], c: bool) -> i64 { let r = if c { a } else { mka(20) }; let q = r; println(f"q{q[1].id}"); 0 }
fn f15(a: R, c: bool) -> R { let r: R = if c { a } else { mkr(8) }; let q = r; println("mid"); q }
fn f16(a: R, c: bool) -> i64 { let r = if c { a } else { mkr(7) }; let r = mkr(20); println(f"r{r.id}"); 0 }
fn main() {
  let k = K { n: 1 };
  let b = f1(mka(1), true); println(f"y{b[0].id}"); let b = f1(mka(3), false); println(f"y{b[0].id}");
  println("-2"); f2(mkr(1), true); f2(mkr(2), false);
  println("-3"); f3(mkr(1), 0); f3(mkr(2), 1); f3(mkr(3), 2);
  println("-4"); f4(mkr(1), true); f4(mkr(2), false);
  println("-5"); f5(mkr(1), mkr(2), true); f5(mkr(3), mkr(4), false);
  println("-6"); f6(mkr(1), true); f6(mkr(2), false);
  println("-7"); f7(mkr(1), true); f7(mkr(2), false);
  println("-8"); f8(H { r: mkr(1), n: 1 }, true); f8(H { r: mkr(2), n: 1 }, false);
  println("-9"); f9(E.A(mkr(1)), true); f9(E.A(mkr(2)), false);
  println("-10"); f10([mkr(1)], true); f10([mkr(2)], false);
  println("-11"); f11((mkr(1), 1), true); f11((mkr(2), 1), false);
  println("-12"); f12(mkr(1), Some(1)); f12(mkr(2), None);
  println("-13"); f13(mka(1), 0); f13(mka(3), 1);
  println("-14"); f14(mka(1), true); f14(mka(3), false);
  println("-15"); let z = f15(mkr(1), true); println(f"z{z.id}"); let w = f15(mkr(2), false); println(f"w{w.id}");
  println("-16"); f16(mkr(1), true); f16(mkr(2), false);
  println("-m"); k.m(mkr(1), true); k.m(mkr(2), false);
  println("end") }
"#,
    ) else {
        return;
    };
    assert_eq!(out, "r1\nd1\nd2\ny30\nd30\nd31\nr8\nd8\nd9\nd3\nd4\ny30\nd30\nd31\n-2\nr1\nd1\nr7\nd7\nd2\n-3\nr1\nd1\nr2\nd2\nr7\nd7\nd3\n-4\nt\nr1\nd1\nr7\nd7\nd2\n-5\nr1\nd2\nd1\nr4\nd4\nd3\n-6\ne1\nd1\ne7\nd7\nd2\n-7\nq1\nd1\nq7\nd7\nd2\n-8\nr1\nd1\nr7\nd7\nd2\n-9\nr1\nd1\nrb\nd2\n-10\nr1\nd1\nr1\nd7\nd2\n-11\nr1\nd1\nr7\nd7\nd2\n-12\nr1\nd1\nr7\nd7\nd2\n-13\nr1\nd1\nd2\nr20\nd20\nd21\nd3\nd4\n-14\nq2\nd1\nd2\nq21\nd20\nd21\nd3\nd4\n-15\nmid\nz1\nd1\nmid\nd2\nw8\nd8\n-16\nr20\nd20\nd1\nd7\nr20\nd20\nd2\n-m\nr1\nd1\nr7\nd7\nd2\nend\n", "got:\n{out}");
}
