//! B-2026-10-04-69: payload handed to a method on a local receiver

use super::*;

/// B-2026-10-04-69: B-2026-10-04-63's remainder. A payload of a by-value
/// `Option` param handed to a method on a LOCAL receiver (`let q2 = Q { .. };
/// match o { Some(r) => q2.eat(r) .. }`) read as a transfer in codegen's payload
/// escape walk, so the caller stood down and the body ran nowhere compiled. The interpreter was already right; this pins it. The
/// walk now types a local from its one binding (a struct literal, an annotation,
/// or a constructor call's declared return type) and asks the method what it does
/// with the argument. Storing receivers (`b.add(r)`, `v.push(r)`) stay stores, and
/// a name rebound by an arm pattern is not typed.
///
/// The storing cells `lb` / `lv` are left out here: `--interp` runs their
/// bodies twice, which is B-2026-09-29-72 (a payload stored into a local
/// container that dies in the callee).
#[test]
fn interp_payload_to_method_on_local_receiver_runs_its_body_once() {
    let out = run(r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: "ab".to_string() + "cd" } }
struct Q { z: i64 }
impl Q {
  fn eat(ref self, s: S) { println("qx") }
  fn new() -> Q { return Q { z: 3 } }
}
struct B { xs: Vec[S] }
impl B {
  fn add(mut ref self, s: S) { self.xs.push(s) }
  fn peek(ref self, s: S) { println(f"pk{self.xs.len()}") }
}
fn mkq() -> Q { return Q { z: 2 } }
fn lo(o: Option[S]) { let q2 = Q { z: 1 }; match o { Some(r) => q2.eat(r), None => {} } println("lo") }
fn lt(o: Option[S]) { let q2: Q = mkq(); match o { Some(r) => q2.eat(r), None => {} } println("lt") }
fn lc(o: Option[S]) { let q2 = mkq(); match o { Some(r) => q2.eat(r), None => {} } println("lc") }
fn ln(o: Option[S]) { let q2 = Q.new(); match o { Some(r) => q2.eat(r), None => {} } println("ln") }
fn lb(o: Option[S]) { let mut b = B { xs: Vec.new() }; match o { Some(r) => b.add(r), None => {} } println(f"lb{b.xs.len()}") }
fn lp(o: Option[S]) { let b = B { xs: Vec.new() }; match o { Some(r) => b.peek(r), None => {} } println("lp") }
fn lv(o: Option[S]) { let mut v: Vec[S] = Vec.new(); match o { Some(r) => v.push(r), None => {} } println(f"lv{v.len()}") }
fn lk(o: Option[S], k: bool) { let q2 = Q { z: 1 }; if k { match o { Some(r) => q2.eat(r), None => {} } } println("lk") }
fn sh(o: Option[S]) { let b = B { xs: Vec.new() }; match o { Some(b) => { let q2 = Q { z: 1 }; q2.eat(b) }, None => {} } println("sh") }
fn main() {
  lo(Some(mks(1))); println("a")
  lt(Some(mks(2))); println("b")
  lc(Some(mks(3))); println("c")
  ln(Some(mks(4))); println("d")
  lp(Some(mks(6))); println("f")
  lk(Some(mks(8)), true); println("h")
  lk(Some(mks(9)), false); println("i")
  sh(Some(mks(10))); println("j")
  println("end")
}
"#);
    assert_eq!(out, "qx\nlo\ndS1\na\nqx\nlt\ndS2\nb\nqx\nlc\ndS3\nc\nqx\nln\ndS4\nd\npk0\nlp\ndS6\nf\nqx\nlk\ndS8\nh\nlk\ndS9\ni\nqx\nsh\ndS10\nj\nend\n");
}
