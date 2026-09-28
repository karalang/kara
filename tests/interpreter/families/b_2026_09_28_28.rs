//! B-2026-09-28-28 — `let Some(x) = a else { .. }` over a by-value
//! `Option`/`Result` param. The statements after it decide whether the binding
//! takes the payload, exactly as an `if let` body does, so a callee that only
//! reads it leaves the caller owing the payload's `Drop` body (design.md rule
//! 3). Compiled builds used to answer "always takes" in the escape analysis and
//! so lost a fresh temp argument's body, and the generic spelling lost it for a
//! named argument too.

use super::*;

/// B-2026-09-28-28 — read-only, conditional-move, consuming, identity
/// hand-back, `Result` and tuple-payload `let .. else` bodies, each called with
/// a fresh temp and a named argument. Before the fix every compiled temp call
/// that only read the binding printed no `d<i>` for it; each body now runs
/// once.
#[test]
fn test_optres_param_let_else_read_only_runs_body_once_at_caller() {
    let out = run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mks(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn id(a: Option[S]) -> Option[S] { a }
fn keep(x: S) { println("kp") }
fn f1(a: Option[S]) -> i64 { let Some(x) = a else { return 0 }; x.r.id }
fn f2(a: Result[S, i64]) -> i64 { let Ok(x) = a else { return 0 }; x.r.id }
fn f3(a: Option[(R, i64)]) -> i64 { let Some(t) = a else { return 0 }; t.1 }
fn f4(a: Option[S]) -> i64 { let Some(x) = a else { return 0 }; if x.r.id > 7 { keep(x); 1 } else { 2 } }
fn f5(a: Option[S]) -> i64 { let Some(x) = id(a) else { return 0 }; x.r.id }
fn f6(a: Option[R]) -> i64 { let Some(x) = a else { return 0 }; x.id }
fn f7(a: Option[S]) -> i64 { let Some(x) = a else { return 0 }; println("mid"); let n = x.r.id; n }
fn f8(a: Option[S]) -> i64 { let Some(x) = a else { return 0 }; keep(x); 1 }
fn main() {
    println(f"k{f1(Some(mks(1)))}"); let a1 = Some(mks(2)); println(f"j{f1(a1)}");
    println(f"k{f2(Ok(mks(3)))}"); let a2: Result[S, i64] = Ok(mks(4)); println(f"j{f2(a2)}");
    println(f"k{f3(Some((R { id: 5 }, 50)))}"); let a3 = Some((R { id: 6 }, 60)); println(f"j{f3(a3)}");
    println(f"k{f4(Some(mks(7)))}"); let a4 = Some(mks(8)); println(f"j{f4(a4)}");
    println(f"k{f5(Some(mks(9)))}"); let a5 = Some(mks(10)); println(f"j{f5(a5)}");
    println(f"k{f6(Some(R { id: 11 }))}"); let a6 = Some(R { id: 12 }); println(f"j{f6(a6)}");
    println(f"k{f7(Some(mks(13)))}"); let a7 = Some(mks(14)); println(f"j{f7(a7)}");
    println(f"k{f8(Some(mks(15)))}"); let a8 = Some(mks(16)); println(f"j{f8(a8)}");
    println("end")
}"#);
    assert_eq!(out, "d1\nk1\nj2\nd2\nd3\nk3\nj4\nd4\nd5\nk50\nj60\nd6\nd7\nk2\nkp\nj1\nd8\nd9\nk9\nj10\nd10\nd11\nk11\nj12\nd12\nmid\nd13\nk13\nmid\nj14\nd14\nkp\nd15\nk1\nkp\nj1\nd16\nend\n");
}

/// B-2026-09-28-28 — the row's own program: a generic `let Some(t) = o else`
/// over `Option[(T, i64)]` lost `dW1/n1` on every compiled surface and leaked
/// the `String` (2 B in 1 block at `-O0`), while the concrete twin was right.
#[test]
fn test_generic_optres_tuple_param_let_else_runs_body_once() {
    let out = run(r#"struct W { id: i64, name: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}/{self.name}") } }
fn mk(i: i64) -> W { return W { id: i, name: f"n{i}" }; }
fn gl[T](o: Option[(T, i64)]) -> i64 { let Some(t) = o else { return 0; }; return t.1; }
fn cl(o: Option[(W, i64)]) -> i64 { let Some(t) = o else { return 0; }; return t.1; }
fn main() { println(f"g{gl(Some((mk(1), 9)))}"); println(f"c{cl(Some((mk(2), 9)))}"); let a = Some((mk(3), 8)); println(f"g{gl(a)}"); println("end") }"#);
    assert_eq!(out, "dW1/n1\ng9\ndW2/n2\nc9\ng8\ndW3/n3\nend\n");
}
