//! B-2026-09-30-50: a param returned through a wrapping local on some paths runs one Drop body per call.

use super::*;

/// B-2026-09-30-50 — a by-value param returned through a WRAPPING local on
/// only some paths runs its `Drop` body once per call on both paths. The
/// tuple and struct locals (`let x = (a, 1); if f { return x } ...`) treated
/// the param's element as a view of the caller's value, while the caller had
/// stood down, so the exit that did not return `x` ran no body. The array
/// local (`let v = [a]`) was not seen by the conditional predicate at all, so
/// the caller never stood down and the returned `v` ran the body a second
/// time. The literal's local now owns the element on both backends, and the
/// array wrap is admitted like the others.
#[test]
fn e2e_param_returned_through_a_wrapping_local_on_some_paths_runs_one_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
struct P { r: R, n: i64 }
struct Q { r: R }
impl Drop for Q { fn drop(mut ref self) { println(f"dQ{self.r.id}") } }
fn ctl(a: R, f: bool) -> (R, i64) { let x = (a, 1); if f { return x; } return (mk(90), 1); }
fn cst(a: R, f: bool) -> P { let x = P { r: a, n: 1 }; if f { return x; } return P { r: mk(91), n: 1 }; }
fn cvl(a: R, f: bool) -> Vec[R] { let v = [a]; if f { return v; } return [mk(92)]; }
fn g2(a: R, b: R, f: bool) -> (R, R) { let x = (a, b); if f { return x; } return (mk(93), mk(94)); }
fn g4(a: R, f: bool) -> (P, i64) { let x = P { r: a, n: 1 }; let y = (x, 2); if f { return y; } return (P { r: mk(95), n: 1 }, 2); }
fn g5(a: R, f: bool) -> Q { let x = Q { r: a }; if f { return x; } return Q { r: mk(96) }; }
fn g6[T](a: T, f: bool, d: T) -> (T, i64) { let x = (a, 1); if f { return x; } return (d, 1); }
fn g8(a: R, f: bool) -> Vec[R] { let v: Vec[R] = [a]; if f { return v; } return [mk(97)]; }
fn g9(a: R, f: bool) -> R { let x = P { r: a, n: 1 }; if f { return x.r; } return mk(98); }
fn main() {
  let t1 = ctl(mk(1), true); println(f"r{t1.1}");
  let t2 = ctl(mk(2), false); println(f"r{t2.1}");
  let s1 = cst(mk(3), true); println(f"r{s1.n}");
  let s2 = cst(mk(4), false); println(f"r{s2.n}");
  let v1 = cvl(mk(5), true); println(f"r{v1.len()}");
  let v2 = cvl(mk(6), false); println(f"r{v2.len()}");
  let a1 = g2(mk(7), mk(8), true); println(f"r{a1.0.id}");
  let a2 = g2(mk(9), mk(10), false); println(f"r{a2.0.id}");
  let c1 = g4(mk(11), true); println(f"r{c1.1}");
  let c2 = g4(mk(12), false); println(f"r{c2.1}");
  let d1 = g5(mk(13), true); println(f"r{d1.r.id}");
  let d2 = g5(mk(14), false); println(f"r{d2.r.id}");
  let e1 = g6(mk(15), true, mk(16)); println(f"r{e1.1}");
  let e2 = g6(mk(17), false, mk(18)); println(f"r{e2.1}");
  let h1 = g8(mk(19), true); println(f"r{h1.len()}");
  let h2 = g8(mk(20), false); println(f"r{h2.len()}");
  let i1 = g9(mk(21), true); println(f"r{i1.id}");
  let i2 = g9(mk(22), false); println(f"r{i2.id}");
  println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "r1\ndR1\ndR2\nr1\ndR90\nr1\ndR3\ndR4\nr1\ndR91\nr1\ndR5\ndR6\nr1\ndR92\nr7\ndR7\ndR8\ndR9\ndR10\nr93\ndR93\ndR94\nr2\ndR11\ndR12\nr2\ndR95\nr13\ndQ13\ndR13\ndQ14\ndR14\nr96\ndQ96\ndR96\ndR16\nr1\ndR15\ndR17\nr1\ndR18\nr1\ndR19\ndR20\nr1\ndR97\nr21\ndR21\ndR22\nr98\ndR98\nend\n", "got:\n{out}");
}
