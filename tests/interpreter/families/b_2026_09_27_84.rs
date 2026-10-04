//! B-2026-09-27-84: whole rebind of a by-value enum param with an Array payload

use super::*;

/// B-2026-09-27-84: a whole rebind `let h = e;` of a by-value param. Over
/// `enum EArr { A(Array[R, 2]), B }`, whose boxed payload runs user `Drop`
/// bodies (caller-sequenced: the caller frees the box and runs the bodies), the
/// let site registered `h` as a second owner of the caller's box, which crashed
/// on every compiled surface. Over `Option[Array[R, 2]]` / `Option[(R, R)]`,
/// whose bodies the callee runs, the rebind was marked a view and ran neither
/// body, and leaked the elements' heap. The cells cover a chain, a `match` on
/// the rebind, a call, a return and a reassignment.
/// The interpreter was already right; this pins it.
#[test]
fn interp_whole_rebind_of_array_payload_enum_param_owns_once() {
    let out = run(r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
enum EArr { A(Array[R, 2]), B }
fn ek(e: EArr) { println("ek") }
fn e1(e: EArr) { let h = e; println("e1"); }
fn e2(e: EArr) { let h = e; let h2 = h; println("e2"); }
fn e3(e: EArr) { let h = e; match h { EArr.A(a) => println(f"e3 {a[0].id}"), EArr.B => println("e3b") } println("e3x"); }
fn e4(e: EArr) { let h = e; ek(h); println("e4"); }
fn e5(e: EArr) -> EArr { let h = e; println("e5"); return h; }
fn e6(e: EArr) { let mut h = e; h = EArr.B; println("e6"); }
fn o1(x: Option[Array[R, 2]]) { let h = x; println("o1"); }
fn o2(x: Option[Array[R, 2]]) { let h = x; let h2 = h; println("o2"); }
fn o3(x: Option[Array[R, 2]]) { let h = x; match h { Some(a) => println(f"o3 {a[1].id}"), None => println("o3n") } println("o3x"); }
fn t1(x: Option[(R, R)]) { let h = x; println("t1"); }
fn t2(x: Option[(R, R)]) { let h = x; let h2 = h; println("t2"); }
fn main() {
  e1(EArr.A([mk(1), mk(2)])); println("a")
  e2(EArr.A([mk(3), mk(4)])); println("b")
  e3(EArr.A([mk(5), mk(6)])); println("c")
  e4(EArr.A([mk(7), mk(8)])); println("d")
  let r5 = e5(EArr.A([mk(9), mk(10)])); println("e")
  e6(EArr.A([mk(11), mk(12)])); println("f")
  o1(Some([mk(21), mk(22)])); println("g")
  o2(Some([mk(23), mk(24)])); println("h")
  o3(Some([mk(25), mk(26)])); println("i")
  t1(Some((mk(41), mk(42)))); println("m")
  t2(Some((mk(43), mk(44)))); println("n")
  println("end")
}
"#);
    assert_eq!(out, "e1\ndR1\ndR2\na\ne2\ndR3\ndR4\nb\ne3 5\ne3x\ndR5\ndR6\nc\nek\ne4\ndR7\ndR8\nd\ne5\ndR9\ndR10\ne\ndR11\ndR12\ne6\nf\no1\ndR21\ndR22\ng\no2\ndR23\ndR24\nh\no3 26\no3x\ndR25\ndR26\ni\nt1\ndR41\ndR42\nm\nt2\ndR43\ndR44\nn\nend\n");
}
