//! B-2026-09-29-43 — a destructured by-value `Option` / `Result` param
//! payload whose arm hands back a SCALAR (or `String`) field bare
//! (`Some(S { r, n, s }) => n`). The bare use scored as the payload escaping,
//! so the caller stood its walk down while the callee owned no destructured
//! field: a `Drop` field's body ran on no backend and a `shared` field
//! leaked its block.

use super::*;

/// B-2026-09-29-43 — `match`, a wildcard field, `if let`, `let .. else`, a
/// rebound scalar, a bare `String` field handed back, and a `Result`, each with a
/// fresh temp and a named argument. Before the fix every body but the named
/// `let .. else` one was lost on both backends.
#[test]
fn test_optres_param_destructured_scalar_field_handed_back_keeps_sibling_bodies() {
    let out = run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, n: i64, s: String }
fn mks(i: i64) -> S { S { r: R { id: i }, n: i, s: f"heap-string-longer-than-sso-{i}" } }
fn f1(a: Option[S]) -> i64 { match a { Some(S { r, n, s }) => n, None => 0 } }
fn f2(a: Option[S]) -> i64 { match a { Some(S { r: _, n, s }) => n, None => 0 } }
fn f3(a: Option[S]) -> i64 { if let Some(S { r, n, s }) = a { n } else { 0 } }
fn f4(a: Option[S]) -> i64 { let Some(S { r, n, s }) = a else { return 0 }; n }
fn f5(a: Option[S]) -> i64 { match a { Some(S { r, n, s }) => { let m = n; return m; } None => 0 } }
fn f6(a: Option[S]) -> String { match a { Some(S { r, n, s }) => s, None => "" } }
fn f7(a: Result[S, i64]) -> i64 { match a { Ok(S { r, n, s }) => n, Err(e) => e } }
fn main() {
    println(f"a{f1(Some(mks(1)))}"); let x1 = Some(mks(2)); println(f"b{f1(x1)}");
    println(f"a{f2(Some(mks(3)))}"); let x2 = Some(mks(4)); println(f"b{f2(x2)}");
    println(f"a{f3(Some(mks(5)))}"); let x3 = Some(mks(6)); println(f"b{f3(x3)}");
    println(f"a{f4(Some(mks(7)))}"); let x4 = Some(mks(8)); println(f"b{f4(x4)}");
    println(f"a{f5(Some(mks(9)))}"); let x5 = Some(mks(10)); println(f"b{f5(x5)}");
    println(f"a{f6(Some(mks(11))).len()}"); let x6 = Some(mks(12)); println(f"b{f6(x6).len()}");
    println(f"a{f7(Ok(mks(13)))}"); let x7: Result[S, i64] = Ok(mks(14)); println(f"b{f7(x7)}");
    println("end")
}"#);
    assert_eq!(out, "d1\na1\nb2\nd2\nd3\na3\nb4\nd4\nd5\na5\nb6\nd6\nd7\na7\nb8\nd8\nd9\na9\nb10\nd10\nd11\na30\nb30\nd12\nd13\na13\nb14\nd14\nend\n");
}
