//! B-2026-09-29-64: an owned-`self` method that returns `match self { .. }`.

use super::*;

/// B-2026-09-29-64 — an owned-`self` method that RETURNS `match self { .. }`
/// destructures its receiver exactly as the tail `match self { .. }` does, so
/// the payload's body runs once, in the arm. Before, `matches_on_scrutinee`
/// walked no `return`, the caller kept its receiver walk armed beside the arm,
/// and the body ran twice on every surface.
#[test]
fn asan_owned_self_method_returning_match_self_runs_the_payload_body_once() {
    assert_clean_asan_run(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"heap-string-longer-than-sso-{i}" } }
enum E { A(S), B(i64) }
impl E {
  fn m1(self) -> i64 { return match self { E.A(s) => s.id, E.B(n) => n } }
  fn m2(self) -> i64 { match self { E.A(s) => s.id, E.B(n) => n } }
  fn m3(self) -> i64 { let k = match self { E.A(s) => s.id, E.B(n) => n }; return k }
  fn m5(self) -> i64 { return match self { E.A(s) => 1, E.B(n) => n } }
  fn m6(self, c: bool) -> i64 { if c { return match self { E.A(s) => s.id + 100, E.B(n) => n } }; return 0 }
  fn m7(self) -> S { return match self { E.A(s) => s, E.B(n) => mks(n) } }
}
fn main() {
  let a = E.A(mks(1)); println(f"m1 {a.m1()}");
  let b = E.A(mks(2)); println(f"m2 {b.m2()}");
  let c = E.A(mks(3)); println(f"m3 {c.m3()}");
  let d = E.A(mks(4)); println(f"m5 {d.m5()}");
  let e = E.A(mks(5)); println(f"m6 {e.m6(true)}");
  let g = E.B(7); println(f"m1 {g.m1()}");
  println(f"tmp {E.A(mks(8)).m1()}");
  let h = E.A(mks(9)); let s = h.m7(); println(f"m7 {s.id}");
  let i = E.B(10); let s2 = i.m7(); println(f"m7 {s2.id}");
  println("end")
}
"#,
        &[
            "dS1", "m1 1", "dS2", "m2 2", "dS3", "m3 3", "dS4", "m5 1", "dS5", "m6 105", "m1 7",
            "dS8", "tmp 8", "m7 9", "dS9", "m7 10", "dS10", "end",
        ],
        "asan_owned_self_method_returning_match_self_runs_the_payload_body_once",
    );
}
