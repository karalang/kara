//! B-2026-09-30-87 — a `?` that converts its error through `From` with a
//! fresh call as its source. The callee's parameter is caller-retained (a bare
//! `String`/`Vec`, an entry-copied struct or enum, or a non-escaping forwarded
//! one), so the `?` site owns the rebuilt source and must drop it after `from`
//! returns; nothing did, leaking the source's heap. A source wider than the
//! payload area is boxed, and the rebuilt argument read the box pointer as its
//! first word (`f6` printed 0 and `f16` an empty string). A transfer-owned
//! parameter keeps the old path: the callee frees it.

use super::*;

/// B-2026-09-30-87 — a fresh `?`-From source is dropped at the `?` site when the callee only borrows it, and a wide boxed source is unboxed before `from` reads it (`f6`, `f16`).
#[test]
fn e2e_question_from_fresh_source_dropped() {
    let src = r#"
enum E1 { A(String), B(i64) }
shared struct Sh { k: i64 }
struct S1 { v: String }
struct S6 { v: String, w: Vec[i64] }
struct S7 { v: String, h: Sh }
struct Q1 { n: i64 }
impl From[S1] for Q1 { fn from(s: S1) -> Q1 { Q1 { n: s.v.len() } } }
struct Q2 { inner: S1, n: i64 }
impl From[S1] for Q2 { fn from(s: S1) -> Q2 { Q2 { inner: s, n: 1 } } }
struct Q3 { n: i64 }
impl From[E1] for Q3 { fn from(e: E1) -> Q3 { match e { E1.A(s) => Q3 { n: s.len() }, E1.B(k) => Q3 { n: k } } } }
struct Q6 { n: i64 }
impl From[S6] for Q6 { fn from(s: S6) -> Q6 { Q6 { n: s.v.len() + s.w.len() } } }
struct Q7 { n: i64 }
impl From[S7] for Q7 { fn from(s: S7) -> Q7 { Q7 { n: s.v.len() + s.h.k } } }
struct Q8 { n: i64 }
impl From[String] for Q8 { fn from(s: String) -> Q8 { Q8 { n: s.len() } } }
struct Q13 { e: E1 }
impl From[E1] for Q13 { fn from(e: E1) -> Q13 { Q13 { e: e } } }
struct Q14 { n: i64 }
impl From[Vec[i64]] for Q14 { fn from(s: Vec[i64]) -> Q14 { Q14 { n: s.len() } } }
struct Q15 { msg: String }
impl From[String] for Q15 { fn from(s: String) -> Q15 { Q15 { msg: s } } }
struct Q16 { inner: S6 }
impl From[S6] for Q16 { fn from(s: S6) -> Q16 { Q16 { inner: s } } }
fn str_of(k: i64) -> String { f"err-heap-string-long-enough-{k}" }
fn e_s1(k: i64) -> Result[i64, S1] { if k > 5 { Result.Err(S1 { v: str_of(k) }) } else { Result.Ok(k) } }
fn e_e1(k: i64) -> Result[i64, E1] { if k > 5 { Result.Err(E1.A(str_of(k))) } else { Result.Ok(k) } }
fn e_s6(k: i64) -> Result[i64, S6] { if k > 5 { Result.Err(S6 { v: str_of(k), w: [1, 2, 3] }) } else { Result.Ok(k) } }
fn e_s7(k: i64) -> Result[i64, S7] { if k > 5 { Result.Err(S7 { v: str_of(k), h: Sh { k: 1 } }) } else { Result.Ok(k) } }
fn e_str(k: i64) -> Result[i64, String] { if k > 5 { Result.Err(str_of(k)) } else { Result.Ok(k) } }
fn e_vec(k: i64) -> Result[i64, Vec[i64]] { if k > 5 { Result.Err([1, 2, 3, k]) } else { Result.Ok(k) } }
fn f1(k: i64) -> Result[i64, Q1] { let x = e_s1(k)?; Result.Ok(x) }
fn f2(k: i64) -> Result[i64, Q2] { let x = e_s1(k)?; Result.Ok(x) }
fn f3(k: i64) -> Result[i64, Q3] { let x = e_e1(k)?; Result.Ok(x) }
fn f6(k: i64) -> Result[i64, Q6] { let x = e_s6(k)?; Result.Ok(x) }
fn f7(k: i64) -> Result[i64, Q7] { let x = e_s7(k)?; Result.Ok(x) }
fn f8(k: i64) -> Result[i64, Q8] { let x = e_str(k)?; Result.Ok(x) }
fn f13(k: i64) -> Result[i64, Q13] { let x = e_e1(k)?; Result.Ok(x) }
fn f14(k: i64) -> Result[i64, Q14] { let x = e_vec(k)?; Result.Ok(x) }
fn f15(k: i64) -> Result[i64, Q15] { let x = e_str(k)?; Result.Ok(x) }
fn f16(k: i64) -> Result[i64, Q16] { let x = e_s6(k)?; Result.Ok(x) }
fn main() {
    match f1(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.n) }
    match f1(2) { Result.Ok(v) => println(v), Result.Err(e) => println(e.n) }
    match f2(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.inner.v) }
    match f3(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.n) }
    match f6(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.n) }
    match f7(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.n) }
    match f8(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.n) }
    match f13(8) { Result.Ok(v) => println(v), Result.Err(e) => match e.e { E1.A(s) => println(s), E1.B(k) => println(k) } }
    match f14(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.n) }
    match f15(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.msg) }
    match f16(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.inner.v) }
    println("end")
}"#;
    let want = "29\n2\nerr-heap-string-long-enough-8\n29\n32\n30\n29\nerr-heap-string-long-enough-8\n4\nerr-heap-string-long-enough-8\nerr-heap-string-long-enough-8\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
