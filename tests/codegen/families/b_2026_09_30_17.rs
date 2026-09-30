//! B-2026-09-30-17 -- param fields moved into a returned array literal run
//! each `Drop` body once.

use super::*;

/// B-2026-09-30-17 -- a by-value struct param's fields moved into an ARRAY
/// literal the callee returns (`fn arr(w: W3) -> Array[R, 2] { return [w.q,
/// w.r]; }`). The part channel counted a tuple literal's elements as handed
/// out and not an array literal's, so the caller's walk over `w` ran both
/// fields' bodies beside the array's owner: `dR1 dR2 dR2 dR1` for one call, on
/// every backend. Covers a discarded, bound and `let _ =` result, a temporary
/// argument, one element, an alias, a tuple element, a field beside a fresh
/// value, a method argument, an owned `self`, and a local's fields (already
/// right).
#[test]
fn e2e_param_fields_in_returned_array_literal_run_each_body_once() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
struct W3 { q: R, r: R }
fn arr(w: W3) -> Array[R, 2] { return [w.q, w.r]; }
fn arr1(w: W3) -> Array[R, 1] { return [w.r]; }
fn arral(w: W3) -> Array[R, 2] { let x = w.q; return [x, w.r]; }
fn arrt(w: W3) -> Array[(R, i64), 1] { return [(w.r, 3)]; }
fn arrmix(w: W3) -> Array[R, 2] { return [w.r, mk(50)]; }
struct Kp { a: i64 }
impl Kp { fn ka(ref self, w: W3) -> Array[R, 2] { return [w.r, w.q]; } }
impl W3 { fn toarr(self) -> Array[R, 2] { return [self.q, self.r]; } }
fn main() {
  println("-c1"); let w = W3 { q: mk(1), r: mk(2) }; arr(w);
  println("-c2"); let w2 = W3 { q: mk(3), r: mk(4) }; let a = arr(w2); println(f"a{a[0].id}{a[1].id}");
  println("-c3"); arr(W3 { q: mk(5), r: mk(6) });
  println("-c4"); let w4 = W3 { q: mk(7), r: mk(8) }; arr1(w4);
  println("-c5"); let w5 = W3 { q: mk(9), r: mk(10) }; let b = arral(w5); println(f"b{b[0].id}");
  println("-c6"); let w6 = W3 { q: mk(11), r: mk(12) }; let c = arrt(w6); println(f"c{c[0].1}");
  println("-c7"); let w7 = W3 { q: mk(13), r: mk(14) }; let d = arrmix(w7); println(f"d{d[1].id}");
  println("-c8"); let w8 = W3 { q: mk(15), r: mk(16) }; let _ = arr(w8);
  println("-c9"); let kp = Kp { a: 1 }; let w9 = W3 { q: mk(17), r: mk(18) }; let e = kp.ka(w9); println(f"e{e[0].id}");
  println("-c10"); let w10 = W3 { q: mk(19), r: mk(20) }; let g = w10.toarr(); println(f"g{g[0].id}");
  println("-c11"); let w11 = W3 { q: mk(21), r: mk(22) }; let h = [w11.q, w11.r]; println(f"h{h[0].id}");
  println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "-c1\ndR1\ndR2\n-c2\na34\ndR3\ndR4\n-c3\ndR5\ndR6\n-c4\ndR8\ndR7\n-c5\nb9\ndR9\ndR10\n-c6\ndR11\nc3\ndR12\n-c7\ndR13\nd50\ndR14\ndR50\n-c8\ndR15\ndR16\n-c9\ne18\ndR18\ndR17\n-c10\ng19\ndR19\ndR20\n-c11\nh21\ndR21\ndR22\nend\n", "got:\n{out}");
}
