//! B-2026-10-01-41: a `let mut` collection reassigned to hold by-value params runs each param's `Drop` body once.

use super::*;

/// B-2026-10-01-41 / B-2026-10-01-42 — a `let mut` local reassigned whole to a
/// collection literal of by-value params (`v = Vec[y]`), or to a local holding
/// one (`let w = Vec[y]; v = w`), holds the caller's values, so their `Drop`
/// bodies run in the caller and nowhere else. A local that STARTED owned
/// (`let mut v = Vec[mk(2)]`) walked the view's elements at scope exit on
/// every surface (`dR2 in1 dR7 dR7`), and `--interp` did the same for one that
/// started as a view (`in1 dR6 dR6 dR5`). Covers both starts, the
/// view-through-a-local spelling, a conditional reassignment taken and not, a
/// view displaced by a fresh value, a fixed `Array`, and a generic body.
#[test]
fn interp_reassigned_mut_collection_param_view_runs_bodies_once() {
    let out = run(r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
fn d2(x: R, y: R) { let mut v = Vec[x]; v = Vec[y]; println(f"in{v.len()}") }
fn d3(y: R) { let mut v = Vec[mk(2)]; v = Vec[y]; println(f"in{v.len()}") }
fn d4(x: R, y: R) { let mut v = Vec[x]; let w = Vec[y]; v = w; println(f"in{v.len()}") }
fn d7(y: R) { let mut v = Vec[mk(3)]; let w = Vec[y]; v = w; println(f"in{v.len()}") }
fn d8(y: R, c: bool) { let mut v = Vec[mk(4)]; if c { v = Vec[y]; } println(f"in{v.len()}") }
fn d9(y: R) { let mut v = Vec[mk(5)]; v = Vec[y]; v = Vec[mk(6)]; println(f"in{v.len()}") }
fn a1(y: R) { let mut v = [mk(7)]; v = [y]; println(f"in{v.len()}") }
fn g4[T](x: T, y: T) { let mut v: Vec[T] = Vec.new(); v = Vec[y]; println(f"in{v.len()}") }
fn main() {
  println("-d2"); d2(mk(10), mk(11)); println("k")
  println("-d3"); d3(mk(12)); println("k")
  println("-d4"); d4(mk(13), mk(14)); println("k")
  println("-d7"); d7(mk(15)); println("k")
  println("-d8t"); d8(mk(16), true); println("k")
  println("-d8f"); d8(mk(17), false); println("k")
  println("-d9"); d9(mk(18)); println("k")
  println("-a1"); a1(mk(19)); println("k")
  println("-g4"); g4(mk(20), mk(21)); println("k")
}
"#);
    assert_eq!(out, "-d2\nin1\ndR11\ndR10\nk\n-d3\ndR2\nin1\ndR12\nk\n-d4\nin1\ndR14\ndR13\nk\n-d7\ndR3\nin1\ndR15\nk\n-d8t\ndR4\nin1\ndR16\nk\n-d8f\nin1\ndR4\ndR17\nk\n-d9\ndR5\nin1\ndR6\ndR18\nk\n-a1\ndR7\nin1\ndR19\nk\n-g4\nin1\ndR21\ndR20\nk\n");
}
