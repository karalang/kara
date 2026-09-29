//! B-2026-09-29-77: an owned-`self` method that matches on `self` on some paths only.

use super::*;

/// B-2026-09-29-77 — an owned-`self` enum method that matches on `self` on
/// SOME paths only runs the receiver's payload body once on every path. The
/// caller stands its payload walk down for the whole call (the arm channel
/// owns the payload), so on a path that never reached the `match` nobody ran
/// it: `if c { match self { .. } } else { 0 }` at `c` false, an early `return`
/// ahead of a top-level `match self`, a non-first `if let`, and B-2026-09-29-64's
/// conditional `return match self { .. }` all printed no body. The callee now
/// adopts the payload bodies alone under a per-path flag that the `match` over
/// `self` clears. `h2` hands `self` to another owned-`self` method on one
/// path, a route the adoption leaves alone, and must stay at one body.
#[test]
fn e2e_owned_self_method_matching_self_on_some_paths_runs_the_payload_body_once() {
    let Some(out) = run_program(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"heap-string-longer-than-sso-{i}" } }
enum E { A(S), B(i64) }
enum F { A(S), B(i64) }
impl Drop for F { fn drop(mut ref self) { println("dF") } }
impl E {
  fn m(self) -> i64 { match self { E.A(s) => s.id, E.B(n) => n } }
  fn t1(self, c: bool) -> i64 { if c { match self { E.A(s) => s.id, E.B(n) => n } } else { 0 } }
  fn t2(self, c: bool) -> i64 { if c { let k = match self { E.A(s) => s.id, E.B(n) => n }; return k }; return 0 }
  fn t3(self, c: bool) -> i64 { if c { return match self { E.A(s) => s.id, E.B(n) => n } }; return 0 }
  fn e1(self, c: bool) -> i64 { if c { return 0 }; match self { E.A(s) => s.id, E.B(n) => n } }
  fn e2(self, c: bool) -> i64 { if c { if let E.A(s) = self { return s.id } }; return 0 }
  fn e3(self, c: bool) -> i64 { let k = if c { match self { E.A(s) => s.id, E.B(n) => n } } else { 7 }; return k }
  fn h2(self, c: bool) -> i64 { if c { return self.m() }; match self { E.A(s) => s.id, E.B(n) => n } }
}
impl F {
  fn f1(self, c: bool) -> i64 { if c { match self { F.A(s) => s.id, F.B(n) => n } } else { 0 } }
}
fn main() {
  let a = E.A(mks(1)); println(f"t1 {a.t1(false)}"); let b = E.A(mks(2)); println(f"t1 {b.t1(true)}");
  let c = E.A(mks(3)); println(f"t2 {c.t2(false)}"); let d = E.A(mks(4)); println(f"t2 {d.t2(true)}");
  let e = E.A(mks(5)); println(f"t3 {e.t3(false)}"); let f = E.A(mks(6)); println(f"t3 {f.t3(true)}");
  let g = E.A(mks(7)); println(f"e1 {g.e1(true)}"); let h = E.A(mks(8)); println(f"e1 {h.e1(false)}");
  let i = E.A(mks(9)); println(f"e2 {i.e2(false)}"); let j = E.A(mks(10)); println(f"e2 {j.e2(true)}");
  let k = E.A(mks(11)); println(f"e3 {k.e3(false)}"); let l = E.A(mks(12)); println(f"e3 {l.e3(true)}");
  let m = E.A(mks(13)); println(f"h2 {m.h2(true)}"); let n = E.A(mks(14)); println(f"h2 {n.h2(false)}");
  let o = F.A(mks(15)); println(f"f1 {o.f1(false)}"); let p = F.A(mks(16)); println(f"f1 {p.f1(true)}");
  println(f"tmp {E.A(mks(17)).t1(false)}"); let q = E.B(18); println(f"t1 {q.t1(false)}");
  println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        "dS1\nt1 0\ndS2\nt1 2\ndS3\nt2 0\ndS4\nt2 4\ndS5\nt3 0\ndS6\nt3 6\ndS7\ne1 0\ndS8\ne1 8\ndS9\ne2 0\ndS10\ne2 10\ndS11\ne3 7\ndS12\ne3 12\ndS13\nh2 13\ndS14\nh2 14\ndS15\nf1 0\ndF\ndS16\nf1 16\ndF\ndS17\ntmp 0\nt1 0\nend\n"
    );
}
