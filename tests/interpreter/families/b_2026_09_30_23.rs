//! B-2026-09-30-23 and B-2026-09-27-71 -- a generic callee's `Array` or
//! tuple result, bound or discarded, runs each element's `Drop` body once.

use super::*;

/// B-2026-09-30-23 and B-2026-09-27-71 -- a GENERIC callee's `Array[T, N]` or
/// tuple result, bound or discarded, runs each element's `Drop` body once.
/// A generic callee has no `fn_return_type_exprs` entry, so the let site
/// recorded no element type for an unannotated `let e = arrg(..)` (no body on
/// any compiled surface, every element's heap leaked, `e[1].id` failed to
/// build), and the discard sites declined the same way; the interpreter
/// declined generic discards on purpose to match. Codegen now binds the
/// callee's `T` for the call (`call_return_te_bound`) and the interpreter's
/// generic gate is gone. Covers bound arrays (params, a param's fields, one
/// element), bound tuples (params, a scalar beside, fields), discarded tuples
/// and arrays (statement, `match` arms, a param's part first and second,
/// `let _ =`) and calls inside a generic caller.
#[test]
fn interp_generic_array_and_tuple_results_run_element_bodies() {
    let out = run(r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
struct G2[T] { q: T, r: T }
fn arrg[T](a: T, b: T) -> Array[T, 2] { return [a, b]; }
fn arrgf[T](w: G2[T]) -> Array[T, 2] { return [w.q, w.r]; }
fn arrm[T](a: T) -> Array[T, 1] { return [a]; }
fn tupg[T](a: T, b: T) -> (T, T) { return (a, b); }
fn tupm[T](a: T, n: i64) -> (T, i64) { return (a, n); }
fn tupf[T](w: G2[T]) -> (T, T) { return (w.q, w.r); }
fn tupp1[T](w: G2[T]) -> (T, i64) { return (w.r, 5); }
fn tupp2[T](w: G2[T]) -> (i64, T) { return (5, w.r); }
fn outer[U](a: U) -> i64 { let t = tupg(mk(60), mk(61)); let e = arrm(mk(62)); return 1; }
fn main() {
  println("-c1"); let e = arrg(mk(1), mk(2)); println(f"r{e[1].id}");
  println("-c2"); let w = G2 { q: mk(3), r: mk(4) }; let f = arrgf(w); println(f"r{f[0].id}");
  println("-c3"); let m = arrm(mk(5)); println(f"r{m[0].id}");
  println("-c4"); let t = tupg(mk(6), mk(7)); println(f"r{t.1.id}");
  println("-c5"); let u = tupm(mk(8), 9); println(f"r{u.0.id}{u.1}");
  println("-c6"); let w2 = G2 { q: mk(10), r: mk(11) }; let g = tupf(w2); println(f"r{g.0.id}");
  println("-c7"); tupm(mk(12), 1); println("r");
  println("-c8"); let n = 1; match n { 1 => tupm(mk(13), 1), _ => tupm(mk(14), 2) }; println("r");
  println("-c9"); arrm(mk(15)); println("r");
  println("-c10"); tupg(mk(16), mk(17)); println("r");
  println("-c11"); let w3 = G2 { q: mk(18), r: mk(19) }; tupp1(w3); println("r");
  println("-c12"); let w4 = G2 { q: mk(20), r: mk(21) }; tupp2(w4); println("r");
  println("-c13"); let _ = arrm(mk(22)); println("r");
  println("-c14"); let _ = tupm(mk(23), 1); println("r");
  println("-c15"); let o = outer(mk(24)); println(f"r{o}");
  println("end")
}
"#);
    assert_eq!(out, "-c1\nr2\ndR1\ndR2\n-c2\nr3\ndR3\ndR4\n-c3\nr5\ndR5\n-c4\nr7\ndR6\ndR7\n-c5\nr89\ndR8\n-c6\nr10\ndR10\ndR11\n-c7\ndR12\nr\n-c8\ndR13\nr\n-c9\ndR15\nr\n-c10\ndR16\ndR17\nr\n-c11\ndR19\ndR18\nr\n-c12\ndR21\ndR20\nr\n-c13\ndR22\nr\n-c14\ndR23\nr\n-c15\ndR60\ndR61\ndR62\ndR24\nr1\nend\n");
}
