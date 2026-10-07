//! B-2026-09-30-22 -- whole by-value params moved into a returned array
//! literal run each `Drop` body once.

use super::*;

/// B-2026-09-30-22 -- a whole by-value param moved into an ARRAY literal the
/// callee returns (`fn arrc(a: R, b: R) -> Array[R, 2] { return [a, b]; }`).
/// The whole-param return predicates counted a tuple literal's elements as
/// handed back and not an array literal's, so the caller's argument ran its
/// body beside the array's owner: `dR2 dR1 dR1 dR2` for one call, on every
/// surface, interpreter included. Covers temporary and named arguments, a
/// discarded result, a conditional `return [..]` on both paths, a block-tail
/// conditional, a fresh element beside the param, an array inside a struct, a
/// tuple and another array, a rebinding, a method, an associated function and
/// a block-tail array.
#[test]
fn interp_whole_params_in_returned_array_literal_run_each_body_once() {
    let out = run(r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
struct Hb { xs: Array[R, 2], n: i64 }
struct K { a: i64 }
impl K {
  fn am(ref self, a: R, b: R) -> Array[R, 2] { return [a, b]; }
  fn st(a: R) -> Array[R, 1] { return [a]; }
}
fn arrc(a: R, b: R) -> Array[R, 2] { return [a, b]; }
fn cnd(a: R, b: R, f: bool) -> Array[R, 2] { if f { return [a, b]; } return [mk(90), mk(91)]; }
fn cndt(a: R, f: bool) -> Array[R, 1] { if f { [a] } else { [mk(92)] } }
fn mix(a: R) -> Array[R, 2] { return [a, mk(50)]; }
fn inst(a: R, b: R) -> Hb { return Hb { xs: [a, b], n: 1 }; }
fn intup(a: R, b: R) -> (Array[R, 2], i64) { return ([a, b], 3); }
fn nest(a: R, b: R, c: R, d: R) -> Array[Array[R, 2], 2] { return [[a, b], [c, d]]; }
fn rb(a: R, b: R) -> Array[R, 2] { let x = a; return [x, b]; }
fn tail(a: R, b: R) -> Array[R, 2] { [a, b] }
fn main() {
  println("-c1"); let c1 = arrc(mk(1), mk(2)); println(f"r{c1[0].id}{c1[1].id}");
  println("-c2"); let p = mk(3); let q = mk(4); let c2 = arrc(p, q); println(f"r{c2[1].id}");
  println("-c3"); arrc(mk(5), mk(6));
  println("-c4"); let c4 = cnd(mk(7), mk(8), true); println(f"r{c4[0].id}{c4[1].id}");
  println("-c5"); let c5 = cnd(mk(9), mk(10), false); println(f"r{c5[0].id}{c5[1].id}");
  println("-c6"); let c6 = cndt(mk(11), true); println(f"r{c6[0].id}");
  println("-c7"); let c7 = cndt(mk(12), false); println(f"r{c7[0].id}");
  println("-c8"); let c8 = mix(mk(13)); println(f"r{c8[0].id}{c8[1].id}");
  println("-c9"); let c9 = inst(mk(14), mk(15)); println(f"r{c9.xs[0].id}{c9.n}");
  println("-c10"); let c10 = intup(mk(16), mk(17)); println(f"r{c10.1}");
  println("-c11"); let c11 = nest(mk(18), mk(19), mk(20), mk(21)); println("r11");
  println("-c12"); let c12 = rb(mk(22), mk(23)); println(f"r{c12[0].id}");
  println("-c13"); let k = K { a: 1 }; let c13 = k.am(mk(24), mk(25)); println(f"r{c13[1].id}");
  println("-c14"); let c14: Array[R, 1] = K.st(mk(26)); println("r14");
  println("-c15"); cnd(mk(29), mk(30), true);
  println("end")
}
"#);
    assert_eq!(out, "-c1\nr12\ndR1\ndR2\n-c2\nr4\ndR3\ndR4\n-c3\ndR5\ndR6\n-c4\nr78\ndR7\ndR8\n-c5\ndR10\ndR9\nr9091\ndR90\ndR91\n-c6\nr11\ndR11\n-c7\ndR12\nr92\ndR92\n-c8\nr1350\ndR13\ndR50\n-c9\nr141\ndR14\ndR15\n-c10\nr3\ndR16\ndR17\n-c11\ndR18\ndR19\ndR20\ndR21\nr11\n-c12\nr22\ndR22\ndR23\n-c13\nr25\ndR24\ndR25\n-c14\ndR26\nr14\n-c15\ndR29\ndR30\nend\n");
}
