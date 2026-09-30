//! B-2026-09-27-72 (and its duplicate B-2026-09-30-35) -- a discarded call
//! whose callee returns `Vec[E]` runs each element's `Drop` body once.

use super::*;

/// B-2026-09-27-72 -- `mv();` over `fn mv() -> Vec[R]` freed the vector and
/// ran no element body on any surface, and `let _ = mv();` ran them under
/// `--interp` only: codegen's discard frees a `Vec` result through
/// `materialize_owned_temp`, which has no bodies peer, and the interpreter's
/// statement arm routed a `Value::Array` only for a declared fixed-`Array`
/// return. Covers the statement and `let _ =` spellings, a param handed back
/// (literal and named argument), whole params moved into a `vec![..]` and a
/// local, associated functions, a heap-free element, a struct element with a
/// `Drop` field, an enum element, a discarded `match` whose arms are such
/// calls, and guards: `Vec[i64]`, `Vec[String]`, an empty result and a bound
/// result.
#[test]
fn e2e_discarded_vec_call_result_runs_element_bodies() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct P { r: R, n: i64 }
enum E { A(R), B }
struct H { a: i64 }
impl H { fn mv(i: i64) -> Vec[R] { return [mk(i), mk(i + 1)]; } }
fn mv(i: i64) -> Vec[R] { return [mk(i), mk(i + 1)]; }
fn pv(v: Vec[R]) -> Vec[R] { return v; }
fn vc(a: R, b: R) -> Vec[R] { return vec![a, b]; }
fn vl(a: R, b: R) -> Vec[R] { let v: Vec[R] = [a, b]; return v; }
fn mkw() -> Vec[W1] { return [W1 { v: 41 }] }
fn mp(i: i64) -> Vec[P] { return [P { r: mk(i), n: 1 }]; }
fn me(i: i64) -> Vec[E] { return [E.A(mk(i)), E.B]; }
fn mi() -> Vec[i64] { return [1, 2]; }
fn ms() -> Vec[String] { return ["a", "b"]; }
fn empty() -> Vec[R] { return []; }
fn main() {
  println("-c1"); mv(1);
  println("-c2"); let _ = mv(3);
  println("-c3"); pv([mk(5), mk(6)]);
  println("-c4"); let q = [mk(7), mk(8)]; pv(q);
  println("-c5"); vc(mk(9), mk(10));
  println("-c6"); vl(mk(11), mk(12));
  println("-c7"); let _ = vl(mk(13), mk(14));
  println("-c8"); H.mv(15);
  println("-c9"); let _ = H.mv(17);
  println("-c10"); mkw();
  println("-c11"); mp(19);
  println("-c12"); me(20);
  println("-c13"); mi();
  println("-c14"); ms();
  println("-c15"); empty();
  println("-c16"); let k = 2; match k { 1 => mv(21), _ => mv(23) };
  println("-c17"); let v = mv(25); println(f"r{v.len()}")
  println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "-c1\ndR1\ndR2\n-c2\ndR3\ndR4\n-c3\ndR5\ndR6\n-c4\ndR7\ndR8\n-c5\ndR9\ndR10\n-c6\ndR11\ndR12\n-c7\ndR13\ndR14\n-c8\ndR15\ndR16\n-c9\ndR17\ndR18\n-c10\ndW1_41\n-c11\ndR19\n-c12\ndR20\n-c13\n-c14\n-c15\n-c16\ndR23\ndR24\n-c17\nr2\ndR25\ndR26\nend\n", "got:\n{out}");
}
