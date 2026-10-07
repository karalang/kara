//! B-2026-09-29-116: a local moved or kept by a call inside a branch that may not run.

use super::*;

/// B-2026-09-29-116 — a local whose container walk runs its payload's `Drop`
/// body keeps that body on the path that never handed it off, when the
/// hand-off inside a branch that may not run is a `let` move or a keeping
/// call. The move retracted the local's payload walk on every path, so
/// `let t = E.A(mks(2)); if c { let y = t }` at `c = false` freed the payload
/// with no body run on every compiled surface. Covers a keeping call, a `let`
/// move in a `then` branch, with an `else`, in a loop body, of an `Option`
/// and of a tuple local, a later match-arm binding of the moved name, and a
/// shadowing `let` of the moved name, each at both values of `c`.
#[test]
fn asan_let_moved_local_in_an_untaken_branch_keeps_its_payload_body() {
    assert_clean_asan_run(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"heap-string-longer-than-sso-{i}" } }
enum E { A(S), B(i64) }
fn keep(e: E) -> E { return e }
fn a1(c: bool) -> i64 { let t = E.A(mks(1)); if c { let k = keep(t); println("k") }; return 0 }
fn a2(c: bool) -> i64 { let t = E.A(mks(2)); if c { let y = t; println("y") }; return 0 }
fn a3(c: bool) -> i64 { let t = E.A(mks(3)); if c { let y = t; println("y") } else { println("n") }; return 0 }
fn a4(c: bool) -> i64 { let t = E.A(mks(4)); let mut i = 0; while i < 1 { if c { let y = t; println("y"); return 1 }; i = i + 1; }; return 0 }
fn a5(c: bool) -> i64 { let t = Option.Some(mks(5)); if c { let y = t; println("y") }; return 0 }
fn a6(c: bool) -> i64 { let t = (mks(6), 1); if c { let y = t; println("y") }; return 0 }
fn a7(c: bool) -> i64 { let o = Option.Some(mks(7)); if c { let y = o; println("y") }; match Option.Some(mks(8)) { Option.Some(o) => { println(f"arm {o.id}") } Option.None => {} }; return 0 }
fn a8(c: bool) -> i64 { let t = E.A(mks(9)); if c { let y = t; println("y") }; let t = E.A(mks(10)); if not c { let z = t; println("z") }; return 0 }
fn main() {
  let mut k = 0;
  while k < 2 {
    let c = k == 1;
    println(f"c {c}");
    println(f"r {a1(c)}"); println(f"r {a2(c)}"); println(f"r {a3(c)}"); println(f"r {a4(c)}");
    println(f"r {a5(c)}"); println(f"r {a6(c)}"); println(f"r {a7(c)}"); println(f"r {a8(c)}");
    k = k + 1;
  }
  println("end")
}
"#,
        &[
            "c false", "dS1", "r 0", "dS2", "r 0", "n", "dS3", "r 0", "dS4", "r 0", "dS5", "r 0",
            "dS6", "r 0", "dS7", "arm 8", "dS8", "r 0", "dS9", "dS10", "z", "r 0", "c true", "dS1",
            "k", "r 0", "dS2", "y", "r 0", "dS3", "y", "r 0", "dS4", "y", "r 1", "dS5", "y", "r 0",
            "dS6", "y", "r 0", "dS7", "y", "arm 8", "dS8", "r 0", "dS9", "y", "dS10", "r 0", "end",
        ],
        "asan_let_moved_local_in_an_untaken_branch_keeps_its_payload_body",
    );
}
