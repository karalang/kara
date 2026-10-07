//! B-2026-10-01-37 — a `?` converting through `From` into a target with TWO
//! `From` impls names its callee by the impl's qualified dispatch segment
//! (`E7@From[S].from`, B-2026-08-27-1), and neither backend's lookup of the
//! callee's AST understood that name. Compiled, the `?` could not tell whether
//! `from` left the source with the caller, so it never dropped it: no `Drop`
//! body and a leaked buffer, fresh and named, either impl order. The
//! interpreter's argument walk made the opposite mistake for a `from` that
//! STORES its source, running the source's body at the `?` and again when the
//! stored copy died.

use super::*;

/// B-2026-10-01-37 — a `?` through one of two `From` impls on one target drops the source once: at the `?` when `from` only reads it, and with the target when `from` stores it.
#[test]
fn asan_question_from_two_impls_drops_source() {
    assert_clean_asan_run_min_allocs(
        r#"struct S { id: i64, v: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
struct T { w: String }
struct E7 { n: i64 }
impl From[T] for E7 { fn from(t: T) -> E7 { E7 { n: t.w.len() } } }
impl From[S] for E7 { fn from(s: S) -> E7 { E7 { n: s.id } } }
struct E8 { n: i64 }
impl From[S] for E8 { fn from(s: S) -> E8 { E8 { n: s.id } } }
impl From[T] for E8 { fn from(t: T) -> E8 { E8 { n: t.w.len() } } }
struct E9 { inner: S }
impl From[S] for E9 { fn from(s: S) -> E9 { E9 { inner: s } } }
impl From[T] for E9 { fn from(t: T) -> E9 { E9 { inner: S { id: 0, v: t.w } } } }
fn mk(k: i64) -> Result[i64, S] { if k > 5 { Result.Err(S { id: k, v: f"err-heap-string-long-enough-{k}" }) } else { Result.Ok(k) } }
fn mt(k: i64) -> Result[i64, T] { if k > 5 { Result.Err(T { w: f"err-heap-string-long-enough-{k}" }) } else { Result.Ok(k) } }
fn h1(k: i64) -> Result[i64, E7] { let r = mk(k); let x = r?; Result.Ok(x) }
fn h2(k: i64) -> Result[i64, E7] { let x = mk(k)?; Result.Ok(x) }
fn h3(k: i64) -> Result[i64, E8] { let x = mk(k)?; Result.Ok(x) }
fn h4(k: i64) -> Result[i64, E7] { let x = mt(k)?; Result.Ok(x) }
fn h5(k: i64) -> Result[i64, E9] { let x = mk(k)?; Result.Ok(x) }
fn main() {
    match h1(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.n) }
    match h1(1) { Result.Ok(v) => println(v), Result.Err(e) => println(e.n) }
    println("-");
    match h2(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.n) }
    println("-");
    match h3(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.n) }
    println("-");
    match h4(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.n) }
    println("-");
    match h5(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.inner.id) }
    println("end")
}"#,
        &[
            "dS8", "8", "1", "-", "dS8", "8", "-", "dS8", "8", "-", "29", "-", "8", "dS8", "end",
        ],
        "asan_question_from_two_impls_drops_source",
        4,
    );
}
