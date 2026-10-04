//! B-2026-09-28-38: method on a borrowed receiver read as a store

use super::*;

/// B-2026-09-28-38: a by-value argument handed to a method called on a BORROWED
/// receiver (`self.eat(s)`, or `q.eat(s)` over `q: ref Q`) read as a store into
/// that receiver, so the caller stood down and the `Drop` body ran nowhere. The
/// store walk now resolves such a call and asks the method what it does with the
/// argument: one that keeps nothing (`fn eat(ref self, s: S)`) leaves the body to
/// the caller, while a method that really stores (`self.xs.push(s)`) is still a
/// store. Cells cover `mut ref self`, a two-argument method, a method handing the
/// argument back, a conditional call, the conditional-return spelling, and the
/// storing method called directly, conditionally and through a `mut ref` param.
#[test]
fn e2e_arg_to_method_on_borrowed_receiver_runs_its_body_once() {
    let Some(out) = run_program(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: "ab".to_string() + "cd" } }
struct Q { z: i64 }
impl Q {
  fn eat(ref self, s: S) { println("qx") }
  fn meat(mut ref self, s: S) { println("mx") }
  fn eat2(ref self, n: i64, s: S) { println(f"q2x{n}") }
  fn back(ref self, s: S) -> S { return s }
  fn only(ref self, s: S) { self.eat(s); println("o") }
  fn monly(mut ref self, s: S) { self.meat(s); println("mo") }
  fn two(ref self, s: S) { self.eat2(3, s); println("t") }
  fn viab(ref self, s: S) { let t = self.back(s); println(f"vb{t.id}") }
  fn cond(ref self, s: S, k: bool) { if k { self.eat(s); } println("c") }
  fn pq(ref self, s: S, k: bool) -> S { if k { return s } self.eat(s); return mks(0) }
}
struct B { xs: Vec[S] }
impl B {
  fn add(mut ref self, s: S) { self.xs.push(s) }
  fn fwd(mut ref self, s: S) { self.add(s); println("fw") }
  fn cfwd(mut ref self, s: S, k: bool) { if k { self.add(s); } println("cf") }
}
fn feat(q: ref Q, s: S) { q.eat(s); println("f") }
fn pv(v: mut ref Vec[S], s: S) { v.push(s); println("pv") }
fn bf(b: mut ref B, s: S) { b.add(s); println("bf") }
fn main() {
  let mut q = Q { z: 0 };
  q.only(mks(1)); println("a")
  let n = mks(2); q.only(n); println("b")
  q.monly(mks(3)); println("c")
  feat(q, mks(4)); println("d")
  q.two(mks(5)); println("e")
  q.viab(mks(6)); println("g")
  q.cond(mks(7), true); println("h")
  q.cond(mks(8), false); println("i")
  let r = q.pq(mks(9), false); println(f"j{r.id}")
  let t = q.pq(mks(10), true); println(f"k{t.id}")
  let mut b = B { xs: Vec.new() };
  b.fwd(mks(11)); println("l")
  b.cfwd(mks(12), true); println("m")
  b.cfwd(mks(13), false); println("n")
  bf(mut b, mks(14)); println("p")
  let mut v: Vec[S] = Vec.new();
  pv(mut v, mks(15)); println(f"r{v.len()}")
  println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "qx\no\ndS1\na\nqx\no\ndS2\nb\nmx\nmo\ndS3\nc\nqx\nf\ndS4\nd\nq2x3\nt\ndS5\ne\nvb6\ndS6\ng\nqx\nc\ndS7\nh\nc\ndS8\ni\nqx\ndS9\nj0\ndS0\nk10\ndS10\nfw\nl\ncf\nm\ncf\ndS13\nn\nbf\ndS11\ndS12\ndS14\np\npv\nr1\ndS15\nend\n", "got:\n{out}");
}
