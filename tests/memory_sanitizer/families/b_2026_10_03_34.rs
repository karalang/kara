//! B-2026-10-03-34 — a generic callee taking `Result[Option[T], E]` by value
//! owns the box one level down, as the non-generic callee does.

use super::*;

/// At `T = R`, `leaf(a)` over `fn leaf[T](x: Result[Option[T], i64]) {
/// match x { Ok(Some(w)) => 1, .. } }` freed R's heap twice: the caller's let
/// site kept the nested box and its leaf drop while the monomorph's arm ran
/// `w`'s own drop. A fresh-temp argument leaked the box (32 B), or the box and
/// R's String when no arm bound the leaf. The monomorph prologue now registers
/// the nested box like `functions.rs`, and the call retracts the caller's half.
///
/// `dR4 1` for the named local is the compiled order the NON-generic callee
/// already gives; the interpreter prints `1 dR4`. PREDICTS B-2026-10-04-3, which
/// covers both paths.
#[test]
fn asan_generic_nested_envelope_param_owned_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn leaf[T](x: Result[Option[T], i64]) -> i64 { match x { Ok(Some(w)) => 1, Ok(None) => 2, Err(e) => e } }
fn env[T](x: Result[Option[T], i64]) -> i64 { match x { Ok(o) => 3, Err(e) => e } }
fn any[T](x: Result[Option[T], i64]) -> i64 { match x { Ok(_) => 4, Err(e) => e } }
fn main() {
    println(leaf(Ok(Some(mk(1)))));
    println(env(Ok(Some(mk(2)))));
    println(any(Ok(Some(mk(3)))));
    let a: Result[Option[R], i64] = Ok(Some(mk(4)));
    println(leaf(a));
    let b: Result[Option[R], i64] = Ok(None);
    println(leaf(b));
    println(env(Err(6)));
    println("end");
}
"#,
        &[
            "dR1", "1", "dR2", "3", "dR3", "4", "dR4", "1", "2", "6", "end",
        ],
        "generic_nested_envelope_param",
    );
}
