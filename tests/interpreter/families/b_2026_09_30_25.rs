//! B-2026-09-30-25 and B-2026-09-30-64: an associated function's `Array` result bound without an annotation.

use super::*;

/// B-2026-09-30-25, B-2026-09-30-64 — an unannotated `let a = K.fr(1)` over an
/// associated function declared `-> Array[R, 1]` records its element type, so
/// the binding runs each element's `Drop` body once and frees its heap, and a
/// field read through an index (`a[0].id`, `K.pa([w])[0].id` bound) builds.
/// The let site's array resolver keyed only a bare-identifier callee, where
/// its tuple twin already keyed `Type.method`. Covers a fresh result, a read,
/// two elements, a hand-back of a literal and of a named array, a generic
/// associated function, a scalar element, the discarded and wildcard
/// spellings, and a rebind.
#[test]
fn interp_associated_fn_array_result_bound_unannotated_runs_element_bodies() {
    let out = run(r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
struct K { a: i64 }
impl K {
  fn fr(i: i64) -> Array[R, 1] { return [mk(i)]; }
  fn f2(i: i64) -> Array[R, 2] { return [mk(i), mk(i + 1)]; }
  fn pa(x: Array[R, 1]) -> Array[R, 1] { return x }
  fn gr[T](x: T) -> Array[T, 1] { return [x] }
  fn sc(i: i64) -> Array[i64, 2] { return [i, i + 1] }
}
fn main() {
  println("-a"); let a = K.fr(1); println("r")
  println("-e"); let e = K.fr(5); println(f"r{e[0].id}")
  println("-t"); let t = K.f2(10); println(f"r{t[1].id}")
  println("-n1"); let w = mk(70); let r = K.pa([w]); println(f"r:{r[0].id}")
  println("-n3"); let b: Array[R, 1] = [mk(20)]; let r3 = K.pa(b); println(f"r:{r3[0].id}")
  println("-g"); let g = K.gr(mk(30)); println(f"r{g[0].id}")
  println("-s"); let s = K.sc(3); println(f"r{s[1]}")
  println("-d"); K.fr(40); println("r")
  println("-w"); let _ = K.fr(41); println("r")
  println("-rb"); let x = K.fr(50); let y = x; println(f"r{y[0].id}")
  println("end")
}
"#);
    assert_eq!(out, "-a\ndR1\nr\n-e\nr5\ndR5\n-t\nr11\ndR10\ndR11\n-n1\nr:70\ndR70\n-n3\nr:20\ndR20\n-g\nr30\ndR30\n-s\nr4\n-d\ndR40\nr\n-w\ndR41\nr\n-rb\nr50\ndR50\nend\n");
}
