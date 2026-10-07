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
fn e2e_let_moved_local_in_an_untaken_branch_keeps_its_payload_body() {
    let Some(out) = run_program(
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
    ) else {
        return;
    };
    assert_eq!(out, "c false\ndS1\nr 0\ndS2\nr 0\nn\ndS3\nr 0\ndS4\nr 0\ndS5\nr 0\ndS6\nr 0\ndS7\narm 8\ndS8\nr 0\ndS9\ndS10\nz\nr 0\nc true\ndS1\nk\nr 0\ndS2\ny\nr 0\ndS3\ny\nr 0\ndS4\ny\nr 1\ndS5\ny\nr 0\ndS6\ny\nr 0\ndS7\ny\narm 8\ndS8\nr 0\ndS9\ny\ndS10\nr 0\nend\n", "got:\n{out}");
}
