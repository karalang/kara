//! B-2026-09-30-68: a discarded generic call whose result is an aggregate only through `T`.

use super::*;

/// B-2026-09-30-68 — a discarded call of a GENERIC user fn whose result is a
/// `Vec` only through its type argument (`pg([mk(40), mk(41)]);` over
/// `fn pg[T](x: T) -> T`, `gv(mk(7));` over `fn gv[T](x: T) -> Vec[T]`) runs
/// each element's `Drop` body once. Codegen's `Vec` discard read only a
/// NON-generic callee's declared return and the interpreter declined every
/// generic callee, so both ran nothing; the `let _ =` and branch spellings
/// already ran the bodies interpreted and none compiled. Covers bare-`T` and
/// `Vec[T]` returns over a literal, a named `Vec`, a user enum,
/// the wildcard and branch spellings, and bound and non-aggregate guards.
#[test]
fn interp_discarded_generic_calls_aggregate_through_t_run_element_bodies() {
    let out = run(r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
enum E { A(R), B }
fn pg[T](x: T) -> T { return x; }
fn gv[T](x: T) -> Vec[T] { return Vec[x] }
fn gfa[T](x: Vec[T]) -> Vec[T] { return x; }
fn gfa2[T](x: T) -> Array[T, 1] { return [x]; }
fn main() {
  println("c3"); pg([mk(40), mk(41)]); println("r");
  println("c5"); gv(mk(7)); println("r");
  println("v1"); let v: Vec[R] = [mk(44)]; pg(v); println("r");
  println("e1"); gv(E.A(mk(47))); println("r");
  println("l1"); let _ = pg([mk(49)]); println("r");
  println("l2"); let _ = gv(mk(50)); println("r");
  println("b1"); let n = 1; match n { 1 => gv(mk(51)), _ => gv(mk(52)) }; println("r");
  println("ga"); let w = [mk(60)]; gfa(w); println("r");
  println("gb"); gfa2(mk(61)); println("r");
  println("ve"); pg(Vec[E.A(mk(62)), E.B]); println("r");
  println("k1"); let k = gv(mk(53)); println(f"k{k.len()}");
  println("s1"); pg(mk(54)); println("r");
  println("end")
}
"#);
    assert_eq!(out, "c3\ndR40\ndR41\nr\nc5\ndR7\nr\nv1\ndR44\nr\ne1\ndR47\nr\nl1\ndR49\nr\nl2\ndR50\nr\nb1\ndR51\nr\nga\ndR60\nr\ngb\ndR61\nr\nve\ndR62\nr\nk1\nk1\ndR53\ns1\ndR54\nr\nend\n");
}
