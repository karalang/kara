//! B-2026-10-04-63: payload or field passed to a method on a borrowed receiver

use super::*;

/// B-2026-10-04-63: B-2026-09-28-38's remainder. A PART of a by-value param, a
/// match-arm payload (`Some(r) => self.eat(r)`) or a field (`self.eat(w.r)`),
/// handed to a method on a borrowed receiver (`self`, or `q` over `q: ref Q`)
/// still read as a store into that receiver, so the caller stood down and the
/// part's `Drop` body ran nowhere. The part walks now resolve such a call and
/// ask the method what it does with the argument; codegen's payload escape map
/// asks the same. Cells cover `if let`, `Result`, a conditional field hand-off,
/// a method that forwards to a reading method, and storing methods called
/// directly, through a forwarder and through a `mut ref` param, which stay stores.
#[test]
fn e2e_payload_to_method_on_borrowed_receiver_runs_its_body_once() {
    let Some(out) = run_program(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: "ab".to_string() + "cd" } }
struct W { r: S, n: i64 }
struct Q { z: i64, keep: Vec[S] }
impl Q {
  fn eat(ref self, s: S) { println("qx") }
  fn add(mut ref self, s: S) { self.keep.push(s) }
  fn fwd(mut ref self, s: S) { self.add(s) }
  fn ofwd(mut ref self, s: S) { self.eat(s) }
  fn po(ref self, o: Option[S]) { match o { Some(r) => self.eat(r), None => {} } println("po") }
  fn pi(ref self, o: Option[S]) { if let Some(r) = o { self.eat(r) } println("pi") }
  fn pr(ref self, o: Result[S, i64]) { match o { Ok(r) => self.eat(r), Err(e) => println(f"e{e}") } println("pr") }
  fn pw(ref self, w: W) { self.eat(w.r); println("pw") }
  fn pc(ref self, w: W, c: bool) { if c { self.eat(w.r) } println("pc") }
  fn sa(mut ref self, o: Option[S]) { match o { Some(r) => self.add(r), None => {} } println("sa") }
  fn sf(mut ref self, o: Option[S]) { match o { Some(r) => self.fwd(r), None => {} } println("sf") }
  fn so(mut ref self, o: Option[S]) { match o { Some(r) => self.ofwd(r), None => {} } println("so") }
  fn sw(mut ref self, w: W) { self.add(w.r); println("sw") }
  fn ret(ref self, o: Option[S]) -> Option[S] { match o { Some(r) => { self.eat2(r); return Some(r) }, None => {} } return None }
  fn eat2(ref self, s: ref S) { println("q2") }
}
fn fo(q: ref Q, o: Option[S]) { match o { Some(r) => q.eat(r), None => {} } println("fo") }
fn fa(q: mut ref Q, o: Option[S]) { match o { Some(r) => q.add(r), None => {} } println("fa") }
fn fw(q: ref Q, w: W) { q.eat(w.r); println("fw") }
fn main() {
  let mut q = Q { z: 0, keep: Vec.new() };
  q.po(Some(mks(1))); println("a");
  q.pi(Some(mks(2))); println("b");
  q.pr(Ok(mks(3))); println("c");
  q.pw(W { r: mks(4), n: 1 }); println("d");
  q.pc(W { r: mks(5), n: 1 }, true); println("e");
  q.pc(W { r: mks(6), n: 1 }, false); println("f");
  q.sa(Some(mks(7))); println("g");
  q.sf(Some(mks(8))); println("h");
  q.so(Some(mks(9))); println("i");
  q.sw(W { r: mks(10), n: 1 }); println("j");
  let k = q.ret(Some(mks(11))); println("k");
  fo(q, Some(mks(12))); println("l");
  fa(mut q, Some(mks(13))); println("m");
  fw(q, W { r: mks(14), n: 1 }); println("n");
  println(f"len{q.keep.len()}");
  println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "qx\npo\ndS1\na\nqx\npi\ndS2\nb\nqx\npr\ndS3\nc\nqx\npw\ndS4\nd\nqx\npc\ndS5\ne\npc\ndS6\nf\nsa\ng\nsf\nh\nqx\nso\ndS9\ni\nsw\nj\nq2\ndS11\nk\nqx\nfo\ndS12\nl\nfa\nm\nqx\nfw\ndS14\nn\nlen4\ndS7\ndS8\ndS10\ndS13\nend\n", "got:\n{out}");
}
