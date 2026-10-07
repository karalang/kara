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
fn asan_arg_to_method_on_borrowed_receiver_runs_its_body_once() {
    assert_clean_asan_run(
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
  q.only(mks(1)); println("a");
  let n = mks(2); q.only(n); println("b");
  q.monly(mks(3)); println("c");
  feat(q, mks(4)); println("d");
  q.two(mks(5)); println("e");
  q.viab(mks(6)); println("g");
  q.cond(mks(7), true); println("h");
  q.cond(mks(8), false); println("i");
  let r = q.pq(mks(9), false); println(f"j{r.id}");
  let t = q.pq(mks(10), true); println(f"k{t.id}");
  let mut b = B { xs: Vec.new() };
  b.fwd(mks(11)); println("l");
  b.cfwd(mks(12), true); println("m");
  b.cfwd(mks(13), false); println("n");
  bf(mut b, mks(14)); println("p");
  let mut v: Vec[S] = Vec.new();
  pv(mut v, mks(15)); println(f"r{v.len()}");
  println("end")
}
"#,
        &[
            "qx", "o", "dS1", "a", "qx", "o", "dS2", "b", "mx", "mo", "dS3", "c", "qx", "f", "dS4",
            "d", "q2x3", "t", "dS5", "e", "vb6", "dS6", "g", "qx", "c", "dS7", "h", "c", "dS8",
            "i", "qx", "dS9", "j0", "dS0", "k10", "dS10", "fw", "l", "cf", "m", "cf", "dS13", "n",
            "bf", "dS11", "dS12", "dS14", "p", "pv", "r1", "dS15", "end",
        ],
        "asan_arg_to_method_on_borrowed_receiver_runs_its_body_once",
    );
}
