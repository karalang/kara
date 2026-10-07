//! B-2026-09-30-18 -- a discarded associated-function call returning a tuple
//! or a fixed array runs its elements' `Drop` bodies.

use super::*;

/// B-2026-09-30-18 -- `H.mint(1);` over `impl H { fn mint(i: i64) -> (R, i64)
/// { .. } }`. The discard's memory walk found the associated function, but
/// its bodies walk admitted only a method tail or a free function whose
/// callee is an `Identifier`, so the compiled surfaces freed the tuple and
/// ran no element body where `--interp` runs one. Covers a fresh and a
/// handed-back element, `let _ =`, a param's field, a fixed array, two
/// `Drop` elements, a nested tuple, a tuple with no `Drop` element, a
/// discarded `match` over two such calls, an enum variant constructor
/// holding a tuple (a `Path` callee that is not a function), and a bound
/// result as a guard.
#[test]
fn asan_discarded_assoc_fn_tuple_and_array_results_run_element_bodies() {
    assert_clean_asan_run(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
struct Ws { r: R, n: i64 }
enum E { A((R, i64)), B }
struct H { a: i64 }
impl H {
  fn mint(i: i64) -> (R, i64) { return (mk(i), 5); }
  fn pass(r: R) -> (R, i64) { return (r, 6); }
  fn sf(w: Ws) -> (R, i64) { return (w.r, 5); }
  fn arr(i: i64) -> Array[R, 2] { return [mk(i), mk(i + 1)]; }
  fn two(i: i64) -> (R, R) { return (mk(i), mk(i + 1)); }
  fn nest(i: i64) -> ((R, i64), R) { return ((mk(i), 1), mk(i + 1)); }
  fn plain(i: i64) -> (String, i64) { return (f"s{i}", i); }
}
fn main() {
  println("-c1"); H.mint(1);
  println("-c2"); H.pass(mk(2));
  println("-c3"); let _ = H.mint(3);
  println("-c4"); let c = Ws { r: mk(4), n: 0 }; H.sf(c);
  println("-c5"); H.arr(5);
  println("-c6"); H.two(7);
  println("-c7"); H.nest(9);
  println("-c8"); H.plain(11);
  println("-c9"); let k = 2; match k { 1 => H.mint(12), _ => H.mint(13) };
  println("-c10"); E.A((mk(14), 1));
  println("-c11"); let t = H.mint(15); println(f"r{t.1}");
  println("end")
}
"#,
        &[
            "-c1", "dR1", "-c2", "dR2", "-c3", "dR3", "-c4", "dR4", "-c5", "dR5", "dR6", "-c6",
            "dR7", "dR8", "-c7", "dR9", "dR10", "-c8", "-c9", "dR13", "-c10", "dR14", "-c11", "r5",
            "dR15", "end",
        ],
        "discarded_assoc_fn_tuple_and_array_results_run_element_bodies",
    );
}
