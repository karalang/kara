//! B-2026-10-01-8: a named `Array` of `Drop` elements handed back by a generic callee.

use super::*;

/// B-2026-10-01-8 — a named `Array[R, 2]` local passed to a generic callee
/// that hands it back whole (`fn pg[T](x: T) -> T`) gives its memory to the
/// result, as the concrete `fn pa(x: Array[R, 2]) -> Array[R, 2]` already did,
/// so the element heap is freed once. The monomorph call never reached the
/// concrete call's hand-back retraction, so the local's drop and the
/// result's both freed it (a double free at -O0, and so under the JIT).
/// Covers the discarded, bound and wildcard spellings, a conditional hand-back
/// whose two exits both return it, a rebind inside the callee, a generic
/// method, a `String` element and a loop, with a tuple-wrapping callee and a
/// callee that keeps the array as guards.
#[test]
fn asan_generic_pass_through_of_named_array_frees_it_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
fn pg[T](x: T) -> T { return x; }
fn pt[T](x: T) -> (T, i64) { return (x, 1); }
fn gk[T](x: T) -> i64 { return 5; }
fn pc[T](x: T, c: bool) -> T { if c { return x; } return x; }
fn pv[T](x: T) -> T { let y = x; return y; }
struct H { n: i64 }
impl H { fn keep[T](ref self, x: T) -> T { return x } }
fn main() {
  println("-a1"); let a: Array[R, 2] = [mk(42), mk(43)]; pg(a); println("r")
  println("-a2"); let b: Array[R, 2] = [mk(44), mk(45)]; let c = pg(b); println(f"r{c[1].id}")
  println("-a6"); let d: Array[R, 2] = [mk(46), mk(47)]; let _ = pg(d); println("r")
  println("-t1"); let e: Array[R, 2] = [mk(48), mk(49)]; let t = pt(e); println(f"r{t.1}")
  println("-k1"); let f: Array[R, 2] = [mk(50), mk(51)]; let k = gk(f); println(f"r{k}")
  println("-c1"); let g: Array[R, 2] = [mk(52), mk(53)]; let gg = pc(g, true); println("r")
  println("-v1"); let h: Array[R, 2] = [mk(54), mk(55)]; let hh = pv(h); println("r")
  println("-m1"); let hz = H { n: 1 }; let m: Array[R, 2] = [mk(56), mk(57)]; let mm = hz.keep(m); println("r")
  println("-s1"); let s: Array[String, 2] = [f"x{1}", f"y{2}"]; let ss = pg(s); println(f"r{ss[0]}")
  println("-l1"); let l: Array[R, 1] = [mk(58)]; for i in 0..2 { let q = pg(mk(60 + i)); } let ll = pg(l); println("r")
}
"#,
        &[
            "-a1", "dR42", "dR43", "r", "-a2", "r45", "dR44", "dR45", "-a6", "dR46", "dR47", "r",
            "-t1", "r1", "dR48", "dR49", "-k1", "dR50", "dR51", "r5", "-c1", "dR52", "dR53", "r",
            "-v1", "dR54", "dR55", "r", "-m1", "dR56", "dR57", "r", "-s1", "rx1", "-l1", "dR60",
            "dR61", "dR58", "r",
        ],
        "asan_generic_pass_through_of_named_array_frees_it_once",
    );
}
