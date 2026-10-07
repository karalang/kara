//! B-2026-10-01-21: reassigning a `let mut` collection literal that holds a by-value param.

use super::*;

/// B-2026-10-01-21 — reassigning a `let mut` local whose collection literal
/// holds a by-value param runs the param's `Drop` body once, in the caller,
/// and the replacement's body where the replacement dies. Compiled, the
/// reassignment ran the displaced param's body inside the callee (the caller
/// then ran it again) and the replacement's body never ran, because the
/// let-site suppression of the view's element bodies was static and outlived
/// the reassignment. Covers `Vec` and array literals, a returned local, a
/// conditional and a loop reassignment, a double reassignment, a push after
/// it, a param in a loop, with a local-built literal (`r5`) as a guard.
#[test]
fn interp_reassigned_param_view_collection_runs_bodies_once() {
    let out = run(r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
fn g7(x: R) { let mut v = Vec[x]; v = Vec[mk(8)]; println("in") }
fn g8(x: R) { let mut v = [x]; v = [mk(8)]; println("in") }
fn f7(x: R) -> Vec[R] { let mut v = Vec[x]; v = Vec[mk(8)]; return v }
fn h7() { let w = mk(3); let mut v = Vec[w]; v = Vec[mk(8)]; println("in") }
fn c1(x: R, c: bool) { let mut v = Vec[x]; if c { v = Vec[mk(8)]; } println(f"in{v.len()}") }
fn c3(x: R) { let mut v = Vec[x]; v = Vec[mk(8)]; v = Vec[mk(9)]; println(f"in{v.len()}") }
fn c4(x: R) { let mut v = Vec[x]; v = Vec[mk(8)]; v.push(mk(9)); println(f"in{v.len()}") }
fn c7(x: R) { let mut a = [x]; if x_ok() { a = [mk(8)]; } println(f"in{a.len()}") }
fn x_ok() -> bool { return true }
fn c8(x: R, c: bool) { let mut v = Vec[x]; while c { v = Vec[mk(8)]; break } println(f"in{v.len()}") }
fn c9(x: R) { let mut v = Vec[x]; println(f"pre{v.len()}"); v = Vec[mk(8)]; println(f"in{v.len()}") }
fn main() {
  println("-r3"); g7(mk(6)); println("k");
  println("-a8"); g8(mk(9)); println("k");
  println("-p7"); let e = f7(mk(17)); println(f"k{e.len()}");
  println("-r5"); h7(); println("k");
  println("-c1"); c1(mk(1), true); println("k");
  println("-c1f"); c1(mk(2), false); println("k");
  println("-c3"); c3(mk(5)); println("k");
  println("-c4"); c4(mk(6)); println("k");
  println("-c7"); c7(mk(10)); println("k");
  println("-c8"); c8(mk(11), true); println("k");
  println("-c8f"); c8(mk(12), false); println("k");
  println("-c9"); c9(mk(13)); println("k");
  println("-l1"); for i in 0..2 { g7(mk(20 + i)) } println("k")
}
"#);
    assert_eq!(out, "-r3\ndR8\nin\ndR6\nk\n-a8\ndR8\nin\ndR9\nk\n-p7\ndR17\nk1\ndR8\n-r5\ndR3\ndR8\nin\nk\n-c1\nin1\ndR8\ndR1\nk\n-c1f\nin1\ndR2\nk\n-c3\ndR8\nin1\ndR9\ndR5\nk\n-c4\nin2\ndR8\ndR9\ndR6\nk\n-c7\nin1\ndR8\ndR10\nk\n-c8\nin1\ndR8\ndR11\nk\n-c8f\nin1\ndR12\nk\n-c9\npre1\nin1\ndR8\ndR13\nk\n-l1\ndR8\nin\ndR20\ndR8\nin\ndR21\nk\n");
}
