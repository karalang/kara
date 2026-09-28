//! B-2026-09-28-7 — a fresh-temp `Option`/`Result` argument to a GENERIC
//! fn's by-value `Option[T]` / `Result[T, E]` param has one owner for its
//! payload's memory, as it does for the non-generic twin.

use super::*;

/// B-2026-09-28-7 — a boxed `Option[S]` (and an `Option[K]` over a user enum)
/// had no caller-side box owner on the generic path, and an inline
/// `Result[S, i64]` / `Result[W, i64]` payload with a `Drop` body inside got
/// neither the monomorph's entry copy nor a caller-side owner, so `S`'s
/// `String` leaked once per call. Output was right throughout; only LSan sees it.
#[test]
fn asan_generic_optres_temp_arg_payload_is_freed_once() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
enum K { A(S), B }
struct W { a: S, b: S }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn mh[T](a: Option[T], c: bool) -> i64 { if c { 1 } else { 2 } }
fn mr[T](a: Result[T, i64], c: bool) -> i64 { if c { 1 } else { 2 } }
fn mb[T](a: Result[T, i64], c: bool) -> Result[T, i64] { if c { a } else { Err(7) } }
fn ma[T](a: Option[T]) -> i64 { match a { Some(_) => 3, None => 0 } }
fn mo[T](a: Result[T, i64]) -> i64 { match a { Ok(_) => 4, Err(e) => e } }
fn ms[T](a: Option[T]) -> i64 { match a { Some(s) => { let t = s; 5 }, None => 0 } }
fn take[T](x: T) -> i64 { 6 }
fn mt[T](a: Option[T]) -> i64 { match a { Some(s) => take(s), None => 0 } }
fn main() {
    println(f"a{mh(Some(mk(1)), false)}");
    println(f"b{mr(Ok(mk(2)), false)}");
    let x = mb(Ok(mk(3)), true);
    println(f"c{mr(Err(9), false)}");
    let y = mb(Ok(mk(4)), false);
    match x { Ok(s) => println(f"got{s.r.id}"), Err(_) => println("e") }
    println(f"f{ma(Some(mk(5)))}");
    println(f"g{mo(Ok(mk(6)))}");
    println(f"h{mh(Some(K.A(mk(7))), false)}");
    println(f"i{mr(Ok(W { a: mk(8), b: mk(9) }), false)}");
    println(f"j{ms(Some(mk(10)))}");
    println(f"k{mt(Some(mk(11)))}");
    println("end")
}
"#,
        &[
            "d1", "a2", "d2", "b2", "c2", "d4", "got3", "d3", "d5", "f3", "d6", "g4", "d7", "h2",
            "d9", "d8", "i2", "d10", "j5", "d11", "k6", "end",
        ],
        "asan_generic_optres_temp_arg_payload_is_freed_once",
        11,
    );
}
