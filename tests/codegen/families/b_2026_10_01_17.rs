//! B-2026-10-01-36 / B-2026-10-01-17 — a `?` on a NAMED `Option`/`Result`
//! (`let r = mk(k); let x = r?;`) moves `r`, and neither backend treated it so.
//! Compiled, the `?` disarmed only word 3 of `r`'s slot, which is the payload's
//! cap only for a bare `String`/`Vec`: a struct payload's drop freed a field
//! under its new owner, and a boxed payload's box drop and the payload-bodies
//! walk read nothing it zeroed, so every struct payload double-freed on both
//! sides of the `?`. The interpreter kept `r`'s payload walk armed and ran a
//! `Drop` body twice. A by-value parameter (`fn h(r: Result[S, S]) { r? }`)
//! ran it twice too, because the caller's walk did not see `r?` as the take a
//! `match r { .. }` is (-36). Through `From`, the named source was read after
//! its binding freed it, or leaked, or read a box pointer as its first field
//! (-17).

use super::*;

/// B-2026-10-01-36 — `let r = mk(k); let x = r?;` moves `r` for every payload shape (struct, boxed, `Drop`-bodied, nested, user enum, in a loop, reassigned, conditional): once per body, no double free.
#[test]
fn e2e_question_named_local_source_moved() {
    let src = r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
shared struct Sh { k: i64, s: String }
struct S1 { n: i64, v: String }
struct S2 { v: String, w: Vec[i64] }
struct S3 { r: R, v: String }
struct S4 { id: i64, v: String }
impl Drop for S4 { fn drop(mut ref self) { println(f"dS{self.id}") } }
struct S5 { h: Sh, v: String }
enum E { A(R), B(String) }
fn hs(k: i64) -> String { f"heap-string-long-enough-{k}" }
fn m1(k: i64) -> Result[S1, i64] { if k > 5 { Result.Ok(S1 { n: k, v: hs(k) }) } else { Result.Err(k) } }
fn m2(k: i64) -> Result[S2, i64] { if k > 5 { Result.Ok(S2 { v: hs(k), w: [1, 2] }) } else { Result.Err(k) } }
fn m3(k: i64) -> Result[S3, i64] { if k > 5 { Result.Ok(S3 { r: R { id: k }, v: hs(k) }) } else { Result.Err(k) } }
fn m4(k: i64) -> Result[S4, i64] { if k > 5 { Result.Ok(S4 { id: k, v: hs(k) }) } else { Result.Err(k) } }
fn m5(k: i64) -> Result[S5, i64] { if k > 5 { Result.Ok(S5 { h: Sh { k: k, s: hs(k) }, v: hs(k) }) } else { Result.Err(k) } }
fn m6(k: i64) -> Option[S4] { if k > 5 { Option.Some(S4 { id: k, v: hs(k) }) } else { Option.None } }
fn m7(k: i64) -> Result[i64, S4] { if k > 5 { Result.Err(S4 { id: k, v: hs(k) }) } else { Result.Ok(k) } }
fn m8(k: i64) -> Result[i64, S3] { if k > 5 { Result.Err(S3 { r: R { id: k }, v: hs(k) }) } else { Result.Ok(k) } }
fn m9(k: i64) -> Option[Option[S4]] { if k > 5 { Option.Some(Option.Some(S4 { id: k, v: hs(k) })) } else { Option.None } }
fn m10(k: i64) -> Result[E, i64] { if k > 5 { Result.Ok(E.A(R { id: k })) } else if k > 3 { Result.Ok(E.B(hs(k))) } else { Result.Err(k) } }
fn h1(k: i64) -> Result[i64, i64] { let r = m1(k); let x = r?; println(x.v); Result.Ok(x.n) }
fn h2(k: i64) -> Result[i64, i64] { let r = m2(k); let x = r?; println(x.v); Result.Ok(x.w.len()) }
fn h3(k: i64) -> Result[i64, i64] { let r = m3(k); let x = r?; println(x.v); Result.Ok(x.r.id) }
fn h4(k: i64) -> Result[i64, i64] { let r = m4(k); let x = r?; println(x.v); Result.Ok(x.id) }
fn h5(k: i64) -> Result[i64, i64] { let r = m5(k); let x = r?; println(x.v); Result.Ok(x.h.k) }
fn h6(k: i64) -> Option[i64] { let r = m6(k); let x = r?; println(x.v); Option.Some(x.id) }
fn h7(k: i64) -> Result[i64, S4] { let r = m7(k); let x = r?; Result.Ok(x) }
fn h8(k: i64) -> Result[i64, S3] { let r = m8(k); let x = r?; Result.Ok(x) }
fn h9(k: i64) -> Option[i64] { let r = m9(k); let x = r?; match x { Option.Some(s) => Option.Some(s.id), Option.None => Option.None } }
fn h10(k: i64) -> Result[i64, i64] { let r = m10(k); let x = r?; match x { E.A(q) => Result.Ok(q.id), E.B(s) => Result.Ok(s.len()) } }
fn h11(n: i64) -> Result[i64, i64] { let mut t = 0; for i in 6..n { let r = m4(i); let x = r?; t = t + x.id; } Result.Ok(t) }
fn h12(k: i64) -> Result[i64, i64] { let mut r = m4(1); r = m4(k); let x = r?; println(x.v); Result.Ok(x.id) }
fn h13(k: i64, c: bool) -> Result[i64, i64] { let r = m4(k); if c { let x = r?; println(x.v); return Result.Ok(x.id) } println("skip"); Result.Ok(0) }
fn pr(r: Result[i64, i64]) { match r { Result.Ok(v) => println(f"ok{v}"), Result.Err(e) => println(f"err{e}") } }
fn po(o: Option[i64]) { match o { Option.Some(v) => println(f"some{v}"), Option.None => println("none") } }
fn main() {
    pr(h1(8))
    pr(h1(1))
    pr(h2(8))
    pr(h3(8))
    pr(h3(1))
    pr(h4(8))
    pr(h5(8))
    po(h6(8))
    po(h6(1))
    match h7(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.v) }
    match h8(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.v) }
    po(h9(8))
    pr(h10(8))
    pr(h10(4))
    pr(h11(9))
    pr(h12(8))
    pr(h13(8, true))
    pr(h13(9, false))
    println("end")
}"#;
    let want = "heap-string-long-enough-8\nok8\nerr1\nheap-string-long-enough-8\nok2\nheap-string-long-enough-8\ndR8\nok8\nerr1\nheap-string-long-enough-8\ndS8\nok8\nheap-string-long-enough-8\nok8\nheap-string-long-enough-8\ndS8\nsome8\nnone\nheap-string-long-enough-8\ndS8\nheap-string-long-enough-8\ndR8\ndS8\nsome8\ndR8\nok8\nok25\ndS6\ndS7\ndS8\nok21\nheap-string-long-enough-8\ndS8\nok8\nheap-string-long-enough-8\ndS8\nok8\ndS9\nskip\nok0\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// B-2026-10-01-36 — `r?` on a by-value parameter takes its payload as `match r { .. }` does, unconditionally and under a branch.
#[test]
fn e2e_question_param_source_moved() {
    let src = r#"
struct S4 { id: i64, v: String }
impl Drop for S4 { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn hs(k: i64) -> String { f"heap-string-long-enough-{k}" }
fn mk(k: i64) -> Result[S4, S4] { if k > 5 { Result.Ok(S4 { id: k, v: hs(k) }) } else { Result.Err(S4 { id: k + 100, v: hs(k) }) } }
fn h(r: Result[S4, S4]) -> Result[i64, S4] { let x = r?; println(x.v); Result.Ok(x.id) }
fn hc(r: Result[S4, S4], c: bool) -> Result[i64, S4] { if c { let x = r?; println(x.v); return Result.Ok(x.id) } println("skip"); Result.Ok(0) }
fn pr(r: Result[i64, S4]) { match r { Result.Ok(v) => println(f"ok{v}"), Result.Err(e) => println(f"err{e.id}") } }
fn main() {
    pr(h(mk(8)))
    pr(h(mk(2)))
    let a = mk(9);
    pr(h(a))
    pr(hc(mk(7), true))
    pr(hc(mk(3), true))
    let b = mk(6);
    pr(hc(b, false))
    println("end")
}"#;
    let want = "heap-string-long-enough-8\ndS8\nok8\nerr102\ndS102\nheap-string-long-enough-9\ndS9\nok9\nheap-string-long-enough-7\ndS7\nok7\nerr103\ndS103\nskip\ndS6\nok0\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// B-2026-10-01-17 — a named source converted through `From` is dropped (and its `Drop` bodies run) at the `?` site, and a boxed one is unboxed before `from` reads it.
#[test]
fn e2e_question_from_named_source() {
    let src = r#"
struct S { id: i64, v: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct T { r: R, v: String }
enum Q { A(R), B(i64) }
impl Drop for Q { fn drop(mut ref self) { println("dQ") } }
fn ms(k: i64) -> S { S { id: k, v: f"err-heap-string-long-enough-{k}" } }
fn e_s(k: i64) -> Result[i64, S] { if k > 5 { Result.Err(ms(k)) } else { Result.Ok(k) } }
fn e_t(k: i64) -> Result[i64, T] { if k > 5 { Result.Err(T { r: R { id: k }, v: f"err-heap-string-long-enough-{k}" }) } else { Result.Ok(k) } }
fn e_q(k: i64) -> Result[i64, Q] { if k > 5 { Result.Err(Q.A(R { id: k })) } else { Result.Ok(k) } }
struct M { k: i64 }
impl M { fn get(self, k: i64) -> Result[i64, S] { e_s(k + self.k) } }
struct E1 { n: i64 }
impl From[S] for E1 { fn from(s: S) -> E1 { E1 { n: s.id } } }
struct E2 { inner: S }
impl From[S] for E2 { fn from(s: S) -> E2 { E2 { inner: s } } }
struct E3 { n: i64, inner: Option[S] }
impl From[S] for E3 { fn from(s: S) -> E3 { if s.id > 100 { E3 { n: 0, inner: Option.Some(s) } } else { E3 { n: s.id, inner: Option.None } } } }
struct E4 { n: i64 }
impl From[S] for E4 { fn from(s: S) -> E4 { let t = s; E4 { n: t.id } } }
struct E5 { n: i64 }
impl From[T] for E5 { fn from(t: T) -> E5 { E5 { n: t.v.len() } } }
struct E6 { n: i64 }
impl From[Q] for E6 { fn from(q: Q) -> E6 { E6 { n: 1 } } }
fn f1(k: i64) -> Result[i64, E1] { let r = e_s(k); let x = r?; Result.Ok(x) }
fn f2(k: i64) -> Result[i64, E2] { let r = e_s(k); let x = r?; Result.Ok(x) }
fn f3(k: i64) -> Result[i64, E3] { let r = e_s(k); let x = r?; Result.Ok(x) }
fn f4(k: i64) -> Result[i64, E4] { let r = e_s(k); let x = r?; Result.Ok(x) }
fn f5(k: i64) -> Result[i64, E5] { let r = e_t(k); let x = r?; Result.Ok(x) }
fn f6(k: i64) -> Result[i64, E6] { let r = e_q(k); let x = r?; Result.Ok(x) }
fn f7(k: i64) -> Result[i64, E1] { let m = M { k: 1 }; let r = m.get(k); let x = r?; Result.Ok(x) }
struct B1 { a: String, b: String }
struct B2 { v: String, w: Vec[i64] }
shared struct Sh { k: i64 }
struct B3 { v: String, h: Sh }
struct E7 { n: i64 }
struct E8 { n: i64 }
struct E9 { n: i64 }
impl From[B1] for E7 { fn from(s: B1) -> E7 { E7 { n: s.a.len() + s.b.len() } } }
impl From[B2] for E8 { fn from(s: B2) -> E8 { E8 { n: s.v.len() + s.w.len() } } }
impl From[B3] for E9 { fn from(s: B3) -> E9 { E9 { n: s.v.len() + s.h.k } } }
fn e_b1(k: i64) -> Result[i64, B1] { if k > 5 { Result.Err(B1 { a: f"err-heap-string-long-enough-{k}", b: "bb" }) } else { Result.Ok(k) } }
fn e_b2(k: i64) -> Result[i64, B2] { if k > 5 { Result.Err(B2 { v: f"err-heap-string-long-enough-{k}", w: [1, 2, 3] }) } else { Result.Ok(k) } }
fn e_b3(k: i64) -> Result[i64, B3] { if k > 5 { Result.Err(B3 { v: f"err-heap-string-long-enough-{k}", h: Sh { k: 5 } }) } else { Result.Ok(k) } }
fn g1(k: i64) -> Result[i64, E7] { let r = e_b1(k); let x = r?; Result.Ok(x) }
fn g2(k: i64) -> Result[i64, E8] { let r = e_b2(k); let x = r?; Result.Ok(x) }
fn g3(k: i64) -> Result[i64, E9] { let r = e_b3(k); let x = r?; Result.Ok(x) }
fn main() {
    match f1(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.n) }
    println("-")
    match f1(2) { Result.Ok(v) => println(v), Result.Err(e) => println(e.n) }
    println("-")
    match f2(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.inner.v) }
    println("-")
    match f3(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.n) }
    println("-")
    match f4(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.n) }
    println("-")
    match f5(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.n) }
    println("-")
    match f6(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.n) }
    println("-")
    match f7(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.n) }
    println("-")
    match g1(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.n) }
    match g2(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.n) }
    match g3(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e.n) }
    match g3(2) { Result.Ok(v) => println(v), Result.Err(e) => println(e.n) }
    println("end")
}"#;
    let want = "dS8\n8\n-\n2\n-\nerr-heap-string-long-enough-8\ndS8\n-\ndS8\n8\n-\ndS8\n8\n-\ndR8\n29\n-\ndQ\ndR8\n1\n-\ndS9\n9\n-\n31\n32\n34\n2\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
