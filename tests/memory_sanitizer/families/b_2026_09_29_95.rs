//! B-2026-09-29-95: a local enum receiver of an owned-self method called in a branch that may not run.

use super::*;

/// B-2026-09-29-95 — a local user enum handed, inside a branch that may not
/// run, as the receiver of an owned-`self` method that takes its payload keeps
/// the payload's `Drop` body on the path that never called. The receiver
/// disarm retracted the local's payload walk on every path, so
/// `let t = E.A(mks(2)); if c { return t.m1() }` at `c = false` freed the
/// payload with no body run on every compiled surface. Covers the call in a
/// `then` branch (shadowing and not), an `else` branch, a loop body, nested
/// branches with a `let .. else` method, a statement call followed by more
/// code, and a payload-free variant, each at both values of `c`.
#[test]
fn asan_receiver_in_an_untaken_branch_keeps_its_payload_body() {
    assert_clean_asan_run(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"heap-string-longer-than-sso-{i}" } }
enum E { A(S), B(i64) }
impl E {
  fn m1(self) -> i64 { return match self { E.A(s) => s.id, E.B(n) => n } }
  fn mb(self) -> i64 { let E.A(s) = self else { return 0 }; return s.id }
}
fn a1(c: bool) -> i64 { let t = E.A(mks(1)); if c { let t = t.m1(); return t }; return 0 }
fn a2(c: bool) -> i64 { let t = E.A(mks(2)); if c { return t.m1() }; return 0 }
fn a3(c: bool) -> i64 { let t = E.A(mks(3)); if c { println("n") } else { let k = t.m1(); println(f"k {k}") }; return 0 }
fn a4(c: bool) -> i64 { let t = E.A(mks(4)); let mut i = 0; while i < 1 { if c { return t.m1() }; i = i + 1; }; return 0 }
fn a5(c: bool, d: bool) -> i64 { let t = E.A(mks(5)); if c { if d { return t.mb() } }; return 0 }
fn a6(c: bool) -> i64 { let t = E.A(mks(6)); if c { let k = t.m1(); println(f"k {k}") }; println("after"); return 0 }
fn a7(c: bool) -> i64 { let t = E.B(7); if c { return t.m1() }; return 0 }
fn main() {
  let mut k = 0;
  while k < 2 {
    let c = k == 1;
    println(f"c {c}")
    println(f"r {a1(c)}"); println(f"r {a2(c)}"); println(f"r {a3(c)}"); println(f"r {a4(c)}")
    println(f"r {a5(c, true)}"); println(f"r {a5(true, c)}"); println(f"r {a6(c)}"); println(f"r {a7(c)}")
    k = k + 1;
  }
  println("end")
}
"#,
        &[
            "c false", "dS1", "r 0", "dS2", "r 0", "dS3", "k 3", "r 0", "dS4", "r 0", "dS5", "r 0",
            "dS5", "r 0", "dS6", "after", "r 0", "r 0", "c true", "dS1", "r 1", "dS2", "r 2", "n",
            "dS3", "r 0", "dS4", "r 4", "dS5", "r 5", "dS5", "r 5", "dS6", "k 6", "after", "r 0",
            "r 7", "end",
        ],
        "asan_receiver_in_an_untaken_branch_keeps_its_payload_body",
    );
}
