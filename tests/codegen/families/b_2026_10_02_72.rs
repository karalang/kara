//! B-2026-10-02-72 -- a match arm that MOVES the inner `Option` out of a
//! boxed nested `Option`/`Result` param frees it once.

use super::*;

/// B-2026-10-02-72 — `Some(inner) => inner` over `Option[Option[R]]`
/// returned the inner `Option[R]` while the param's box drop still freed the
/// box under it and R's heap: `free(): double free` (generic callee) or
/// silent use-after-free output (concrete callee). The same held for a
/// `String` leaf (`Option[Option[String]]`, `Result[Option[String], _]`,
/// three levels deep) and for a `let`-bound scrutinee. An arm that only
/// reads the binding (`peeks`, `readr`) keeps the box as the owner.
#[test]
fn e2e_arm_moving_inner_option_out_of_boxed_nested_option() {
    let src = r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn hs(i: i64) -> String { return f"heap-string-longer-than-sso-{i}" }
fn take(x: Option[Option[R]]) -> Option[R] {
    match x {
        Some(inner) => inner,
        None => None,
    }
}
fn takeg[T](x: Option[Option[T]]) -> Option[T] {
    match x {
        Some(inner) => inner,
        None => None,
    }
}
fn takes(x: Option[Option[String]]) -> Option[String] {
    match x {
        Some(inner) => inner,
        None => None,
    }
}
fn taker(x: Result[Option[String], i64]) -> Option[String] {
    match x {
        Ok(inner) => inner,
        Err(_) => None,
    }
}
fn take3(x: Option[Option[Option[String]]]) -> Option[Option[String]] {
    match x {
        Some(inner) => inner,
        None => None,
    }
}
fn peeks(x: Option[Option[String]]) -> i64 {
    match x {
        Some(inner) => 7,
        None => 0,
    }
}
fn readr(x: Option[Option[R]]) -> i64 {
    match x {
        Some(inner) => 8,
        None => 0,
    }
}
fn main() {
    let a = take(Some(Some(mk(1))));
    match a { Some(v) => println(v.id), None => println("none") }
    let b = takeg(Some(Some(mk(2))));
    match b { Some(v) => println(v.id), None => println("none") }
    let c: Option[Option[R]] = Some(Some(mk(3)));
    let d = take(c);
    match d { Some(v) => println(v.id), None => println("none") }
    match takes(Some(Some(hs(4)))) { Some(v) => println(v), None => println("none") }
    match taker(Ok(Some(hs(5)))) { Some(v) => println(v), None => println("none") }
    match take3(Some(Some(Some(hs(6))))) { Some(Some(v)) => println(v), _ => println("none") }
    println(peeks(Some(Some(hs(7)))));
    println(readr(Some(Some(mk(8)))));
    let e: Option[Option[String]] = Some(Some(hs(9)));
    let f = match e { Some(inner) => inner, None => None };
    match f { Some(v) => println(v), None => println("none") }
    println("end")
}
"#;
    let want = "1\ndR1\n2\ndR2\n3\ndR3\nheap-string-longer-than-sso-4\nheap-string-longer-than-sso-5\nheap-string-longer-than-sso-6\n7\ndR8\n8\nheap-string-longer-than-sso-9\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
