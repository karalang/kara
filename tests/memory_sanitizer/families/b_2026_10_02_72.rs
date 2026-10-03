//! B-2026-10-02-72 -- a match arm that MOVES the inner `Option` out of a
//! boxed nested `Option`/`Result` param frees it once.

use super::*;

/// B-2026-10-02-72 — the memory half: one free of the inner box and the
/// leaf's heap, by the result, and none by the param's box drop. Same
/// program as the codegen twin.
#[test]
fn asan_arm_moving_inner_option_out_of_boxed_nested_option() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
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
    println(peeks(Some(Some(hs(7)))))
    println(readr(Some(Some(mk(8)))))
    let e: Option[Option[String]] = Some(Some(hs(9)));
    let f = match e { Some(inner) => inner, None => None };
    match f { Some(v) => println(v), None => println("none") }
    println("end")
}
"#,
        &[
            "1",
            "dR1",
            "2",
            "dR2",
            "3",
            "dR3",
            "heap-string-longer-than-sso-4",
            "heap-string-longer-than-sso-5",
            "heap-string-longer-than-sso-6",
            "7",
            "dR8",
            "8",
            "heap-string-longer-than-sso-9",
            "end",
        ],
        "arm_moving_inner_option_out",
    );
}

/// B-2026-10-02-72 — a GUARDED arm keeps main's behaviour: the retraction is
/// static, and the guard can fail into an arm that leaves the box to the
/// param, so cutting the chain there leaked it (61 B at -O0).
#[test]
fn asan_guarded_arm_over_boxed_nested_option_frees_on_fallthrough() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn guard(x: Option[Option[R]], c: bool) -> Option[R] {
    match x {
        Some(inner) if c => inner,
        _ => None,
    }
}
fn main() {
    let h = guard(Some(Some(mk(4))), false);
    println("h");
    println("end")
}
"#,
        &["dR4", "h", "end"],
        "guarded_arm_nested_option_fallthrough",
    );
}
