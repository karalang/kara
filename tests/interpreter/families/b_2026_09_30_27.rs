//! B-2026-09-30-27 -- whole by-value params moved into a LOCAL array or
//! `Vec` that is then returned run each `Drop` body once.

use super::*;

/// B-2026-09-30-27 -- a whole by-value param moved into a local array or
/// `Vec` literal (`let v: Vec[R] = [a, b]; return v`) and handed back through
/// that local. The wrap-alias walk recorded only struct and tuple literals, so
/// the all-paths predicate did not see `return v` as handing `a` back and the
/// caller's argument ran its body beside the result's owner, on every
/// surface. Covers the `Vec`-typed, `Array`-typed, `vec![..]`, unannotated
/// and `Vec[..]` spellings, a rebind of the local, a param nested in a struct
/// element, a fresh element beside the param, a named argument, an
/// associated function, a block tail, a generic callee, a discarded array
/// local, and the not-taken path of a conditional return (a guard).
#[test]
fn interp_whole_params_in_returned_local_array_or_vec_run_each_body_once() {
    let out = run(r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
struct P { r: R, n: i64 }
struct H { a: i64 }
impl H { fn m(a: R, b: R) -> Vec[R] { let v = [a, b]; return v; } }
fn vl(a: R, b: R) -> Vec[R] { let v: Vec[R] = [a, b]; return v; }
fn al(a: R, b: R) -> Array[R, 2] { let x: Array[R, 2] = [a, b]; return x; }
fn vm(a: R, b: R) -> Vec[R] { let v = vec![a, b]; return v; }
fn vu(a: R, b: R) -> Vec[R] { let v = [a, b]; return v; }
fn vr(a: R, b: R) -> Vec[R] { let v = [a, b]; let w = v; return w; }
fn vn(a: R) -> Vec[P] { let v: Vec[P] = [P { r: a, n: 1 }]; return v; }
fn vf(a: R) -> Vec[R] { let v = [a, mk(30)]; return v; }
fn vt(a: R, b: R) -> Vec[R] { let v = [a, b]; v }
fn cvl(a: R, f: bool) -> Vec[R] { let v = [a]; if f { return v; } return [mk(90)]; }
fn vp(a: R) -> Vec[R] { let v = Vec[a]; v }
fn gl[T](a: T, b: T) -> Vec[T] { let v = [a, b]; return v; }
fn main() {
  println("-c1"); let v1 = vl(mk(1), mk(2)); println(f"r{v1.len()}")
  println("-c2"); let v2 = al(mk(3), mk(4)); println(f"r{v2[0].id}")
  println("-c3"); let v3 = vm(mk(5), mk(6)); println(f"r{v3.len()}")
  println("-c4"); let v4 = vu(mk(7), mk(8)); println(f"r{v4.len()}")
  println("-c5"); let v5 = vr(mk(9), mk(10)); println(f"r{v5.len()}")
  println("-c6"); let v6 = vn(mk(11)); println(f"r{v6.len()}")
  println("-c7"); let v7 = vf(mk(12)); println(f"r{v7.len()}")
  println("-c8"); let c = mk(13); let v8 = vl(c, mk(14)); println(f"r{v8.len()}")
  println("-c10"); let v10 = H.m(mk(17), mk(18)); println(f"r{v10.len()}")
  println("-c11"); let v11 = vt(mk(19), mk(20)); println(f"r{v11.len()}")
  println("-c13"); let v13 = cvl(mk(22), false); println(f"r{v13.len()}")
  println("-c14"); let v14 = gl(mk(23), mk(24)); println(f"r{v14.len()}")
  println("-c15"); let v15 = vp(mk(25)); println(f"r{v15.len()}")
  println("-c16"); al(mk(26), mk(27));
  println("end")
}
"#);
    assert_eq!(out, "-c1\nr2\ndR1\ndR2\n-c2\nr3\ndR3\ndR4\n-c3\nr2\ndR5\ndR6\n-c4\nr2\ndR7\ndR8\n-c5\nr2\ndR9\ndR10\n-c6\nr1\ndR11\n-c7\nr2\ndR12\ndR30\n-c8\nr2\ndR13\ndR14\n-c10\nr2\ndR17\ndR18\n-c11\nr2\ndR19\ndR20\n-c13\ndR22\nr1\ndR90\n-c14\nr2\ndR23\ndR24\n-c15\nr1\ndR25\n-c16\ndR26\ndR27\nend\n");
}
