//! B-2026-09-29-22 -- storing into an `Option` / `Result` field runs the
//! displaced payload's `Drop` body on every backend.

use super::*;

/// B-2026-09-29-22 -- storing into an `Option` / `Result` field
/// (`o.u = None` over `O { u: Option[S2] }`) ran no `Drop` body for the
/// displaced `Some` payload on any backend, the interpreter included. Both
/// displaced-field gates took the field's value by HEAD name and refused
/// `Option` / `Result`, so they agreed and no differential oracle caught it.
/// Covers `Some` over `Some`, `Result` swaps, a method, a `mut ref` param, a
/// two-level chain, an `Option[Vec[S2]]` payload, a field that starts `None`,
/// a loop, and an `Option[i64]` field that runs nothing.
#[test]
fn e2e_optres_field_store_runs_displaced_payload_body() {
    let Some(out) = run_program(
        r#"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mk(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
struct O { u: Option[S2] }
struct O2 { u: Option[S2], n: i64 }
struct Rs { u: Result[S2, S2] }
struct P { o: O }
struct Ov { u: Option[Vec[S2]] }
struct Oi { u: Option[i64] }
impl O { fn clr(mut ref self) { self.u = None; } }
fn clr(o: mut ref O) { o.u = None; }
fn c1() { let mut o = O { u: Some(mk(9)) }; o.u = None; println("x") }
fn c2() { let mut o = O { u: Some(mk(9)) }; o.u = Some(mk(8)); println(f"x{o.u.is_some()}") }
fn c3() { let mut o = O2 { u: Some(mk(9)), n: 3 }; o.u = None; println(f"x{o.n}") }
fn c4() { let mut o = Rs { u: Result.Ok(mk(9)) }; o.u = Result.Err(mk(8)); println("x"); o.u = Result.Ok(mk(7)); println("y") }
fn c5() { let mut o = O { u: Some(mk(9)) }; o.clr(); println("x") }
fn c6() { let mut o = O { u: Some(mk(9)) }; clr(mut o); println("x") }
fn c7() { let mut p = P { o: O { u: Some(mk(9)) } }; p.o.u = None; println("x") }
fn c8() { let mut o = Ov { u: Some([mk(9), mk(8)]) }; o.u = None; println("x") }
fn c9() { let mut o = O { u: None }; o.u = Some(mk(9)); println("x"); o.u = Some(mk(8)); println("y") }
fn c10() { let mut o = Oi { u: Some(4) }; o.u = None; println(f"x{o.u.is_none()}") }
fn c11() { let mut o = O { u: Some(mk(9)) }; let mut i = 0; while i < 3 { o.u = Some(mk(i)); i = i + 1; } println("x") }
fn main() {
  println("-c1"); c1();
  println("-c2"); c2();
  println("-c3"); c3();
  println("-c4"); c4();
  println("-c5"); c5();
  println("-c6"); c6();
  println("-c7"); c7();
  println("-c8"); c8();
  println("-c9"); c9();
  println("-c10"); c10();
  println("-c11"); c11();
  println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "-c1\ndS9\nx\n-c2\ndS9\nxtrue\ndS8\n-c3\ndS9\nx3\n-c4\ndS9\nx\ndS8\ndS7\ny\n-c5\ndS9\nx\n-c6\ndS9\nx\n-c7\ndS9\nx\n-c8\ndS9\ndS8\nx\n-c9\nx\ndS9\ndS8\ny\n-c10\nxtrue\n-c11\ndS9\ndS0\ndS1\ndS2\nx\nend\n", "got:\n{out}");
}
