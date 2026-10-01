//! B-2026-10-01-38 — a `shared` value as the payload a `match` or a `?` takes
//! out of a `Result` leaked its handle on every compiled surface. Three causes:
//! the inline-payload suppression zeroed the payload words and so disarmed the
//! slot's tag-guarded `RcDecOption` after the arm binding had taken its own
//! reference; a fresh `f(k)?` and a named `r?` whose slot was moved both
//! re-incremented a handle that was already the binding's; and a `?` through
//! `From` left the caller's reference to the source behind when `from` takes
//! its own.
//!
//! The `From` cells here keep to an `E2` that only reads its source: an `Err`
//! payload struct whose only heap is a `shared` field (`struct E3 { inner: Sh
//! }`) leaks 40 B when a `match` binds it, with or without a `?` — a separate
//! defect, filed on its own row, and covered for output only by the codegen
//! twin of this file.

use super::*;

/// B-2026-10-01-38 — a `shared` `Ok`/`Some` payload through `match`, `?` (named and fresh) and a by-value param is released once.
#[test]
fn asan_shared_result_payload_match_and_question_release() {
    assert_clean_asan_run_min_allocs(
        r#"shared struct Sh { k: i64, s: String }
fn hs(k: i64) -> String { f"heap-string-long-enough-{k}" }
fn mk(k: i64) -> Result[Sh, String] { if k > 5 { Result.Ok(Sh { k: k, s: hs(k) }) } else { Result.Err(hs(k)) } }
fn mi(k: i64) -> Result[Sh, i64] { if k > 5 { Result.Ok(Sh { k: k, s: hs(k) }) } else { Result.Err(k) } }
fn mo(k: i64) -> Option[Sh] { if k > 5 { Option.Some(Sh { k: k, s: hs(k) }) } else { Option.None } }
fn pr(r: Result[i64, String]) { match r { Result.Ok(v) => println(v), Result.Err(e) => println(e) } }
fn pi(r: Result[i64, i64]) { match r { Result.Ok(v) => println(v), Result.Err(e) => println(e) } }
fn po(r: Option[i64]) { match r { Option.Some(v) => println(v), Option.None => println(0) } }
fn m1(k: i64) -> Result[i64, String] { let r = mk(k); match r { Result.Ok(x) => Result.Ok(x.k), Result.Err(e) => Result.Err(e) } }
fn m2(k: i64) -> i64 { match mk(k) { Result.Ok(x) => x.k, Result.Err(e) => e.len() } }
fn m3(k: i64) -> Result[i64, String] { let r = mk(k); match r { Result.Ok(x) => { let y = x; Result.Ok(y.k) }, Result.Err(e) => Result.Err(e) } }
fn q1(k: i64) -> Result[i64, String] { let r = mk(k); let x = r?; Result.Ok(x.k) }
fn q2(k: i64) -> Result[i64, String] { let x = mk(k)?; let y = x; Result.Ok(y.k) }
fn q3(k: i64) -> Option[i64] { let x = mo(k)?; Option.Some(x.k) }
fn q4(k: i64) -> Result[i64, i64] { let r = mi(k); let x = r?; Result.Ok(x.k) }
fn q5(k: i64) -> Result[Sh, String] { let r = mk(k); let x = r?; Result.Ok(x) }
fn q6(k: i64, v: mut ref Vec[Sh]) -> Result[i64, String] { let r = mk(k); let x = r?; v.push(x); Result.Ok(v.len()) }
fn q7(n: i64) -> Result[i64, String] { let mut t = 0; for i in 6..n { let r = mk(i); let x = r?; t = t + x.k; } Result.Ok(t) }
fn q8(r: Result[Sh, String]) -> Result[i64, String] { let x = r?; Result.Ok(x.k) }
fn q9(r: Result[Sh, i64]) -> Result[i64, i64] { let x = r?; Result.Ok(x.k) }
fn q10(k: i64) -> Option[i64] { let r = mo(k); let x = r?; Option.Some(x.k) }
fn main() {
    pr(m1(8))
    pr(m1(1))
    println(m2(8))
    pr(m3(8))
    pr(q1(8))
    pr(q1(1))
    pr(q2(8))
    po(q3(8))
    po(q3(1))
    pi(q4(8))
    pi(q4(1))
    match q5(8) { Result.Ok(s) => println(s.k), Result.Err(e) => println(e) }
    let mut v: Vec[Sh] = Vec.new();
    pr(q6(8, mut v))
    pr(q6(9, mut v))
    pr(q6(1, mut v))
    println(v[1].k)
    pr(q7(9))
    pr(q8(mk(8)))
    let a = mk(9);
    pr(q8(a))
    pi(q9(mi(7)))
    po(q10(8))
    println("end")
}"#,
        &[
            "8",
            "heap-string-long-enough-1",
            "8",
            "8",
            "8",
            "heap-string-long-enough-1",
            "8",
            "8",
            "0",
            "8",
            "1",
            "8",
            "1",
            "2",
            "heap-string-long-enough-1",
            "9",
            "21",
            "8",
            "9",
            "7",
            "8",
            "end",
        ],
        "asan_shared_result_payload_match_and_question_release",
        4,
    );
}

/// B-2026-10-01-38 — a `shared` `Err` payload through `?` and a `From` that reads it is released once.
#[test]
fn asan_shared_err_payload_question_from_release() {
    assert_clean_asan_run_min_allocs(
        r#"shared struct Sh { k: i64, s: String }
fn hs(k: i64) -> String { f"heap-string-long-enough-{k}" }
struct E2 { n: i64 }
impl From[Sh] for E2 { fn from(s: Sh) -> E2 { E2 { n: s.s.len() } } }
fn mk(k: i64) -> Result[i64, Sh] { if k > 5 { Result.Err(Sh { k: k, s: hs(k) }) } else { Result.Ok(k) } }
fn f1(k: i64) -> Result[i64, E2] { let r = mk(k); let x = r?; Result.Ok(x) }
fn f2(k: i64) -> Result[i64, E2] { let x = mk(k)?; Result.Ok(x) }
fn f5(n: i64) -> Result[i64, E2] { let mut t = 0; for i in 1..n { let x = mk(i)?; t = t + x; } Result.Ok(t) }
fn f6(r: Result[i64, Sh]) -> Result[i64, E2] { let x = r?; Result.Ok(x) }
fn p2(r: Result[i64, E2]) { match r { Result.Ok(v) => println(v), Result.Err(e) => println(e.n) } }
fn main() {
    p2(f1(8))
    p2(f1(2))
    p2(f2(8))
    p2(f5(9))
    p2(f6(mk(8)))
    let a = mk(9);
    p2(f6(a))
    println("end")
}"#,
        &["25", "2", "25", "25", "25", "25", "end"],
        "asan_shared_err_payload_question_from_release",
        4,
    );
}
