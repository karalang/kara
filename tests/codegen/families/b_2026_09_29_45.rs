//! B-2026-09-29-45 -- a fresh `Option` / `Result` temp lent to a `ref` param
//! runs its payload's `Drop` body as the call returns, and frees its box.

use super::*;

/// B-2026-09-29-45 — `g(Some(mk(6)))` over `fn g(x: ref Option[R])` ran no
/// body on any compiled surface, whatever the payload's layout: a boxed payload
/// also leaked its box, an inline one freed its memory alone. Covered: a boxed
/// and an inline payload, a `Result`, a discarded call, a producer call
/// (`mko(12)`), a method and an ASSOCIATED fn (whose struct argument
/// `H.peekr(mk(15))` had the same gap), two arguments (right to left), a loop,
/// and `None`.
#[test]
fn e2e_fresh_optres_temp_lent_to_ref_param_runs_its_body_at_the_call() {
    let src = r#"struct R { name: String, id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.name}") } }
struct S { id: i64 }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mk(n: i64, s: String) -> R { R { name: s, id: n } }
fn mko(n: i64) -> Option[S] { Some(S { id: n }) }
fn mkr(n: i64) -> Result[R, i64] { Ok(mk(n, f"r-heap-string-longer-than-sso")) }
fn g1(x: ref Option[R]) -> i64 { match x { Some(r) => r.id, None => 0 } }
fn g0(x: ref Option[R]) -> i64 { 2 }
fn gr(x: ref Result[R, i64]) -> i64 { 3 }
fn gs(x: ref Option[S]) -> i64 { 4 }
fn two(a: ref Option[R], b: ref Option[R]) -> i64 { 9 }
struct H { k: i64 }
impl H {
    fn look(ref self, x: ref Option[R]) -> i64 { self.k }
    fn peeko(x: ref Option[R]) -> i64 { 8 }
    fn peekr(x: ref R) -> i64 { x.id }
}
fn main() {
    println(f"n{g1(Some(mk(6, f"f-heap-string-longer-than-sso")))}");
    println(f"n{g0(Some(mk(7, f"g")))}");
    g0(Some(mk(8, f"h")));
    println(f"n{gr(mkr(9))}");
    println(f"n{gr(Ok(mk(10, f"j")))}");
    println(f"n{gs(Some(S { id: 11 }))}");
    println(f"n{gs(mko(12))}");
    let h = H { k: 7 };
    println(f"n{h.look(Some(mk(13, f"m")))}");
    println(f"n{H.peeko(Some(mk(14, f"n")))}");
    println(f"n{H.peekr(mk(15, f"o"))}");
    println(f"n{two(Some(mk(16, f"p")), Some(mk(17, f"q")))}");
    for i in 0..2 { println(f"n{two(Some(mk(i, f"l")), None)}") }
    println(f"n{g0(None)}");
    println("end")
}
"#;
    let want = "dR6/f-heap-string-longer-than-sso\nn6\ndR7/g\nn2\ndR8/h\ndR9/r-heap-string-longer-than-sso\nn3\ndR10/j\nn3\ndS11\nn4\ndS12\nn4\ndR13/m\nn7\ndR14/n\nn8\ndR15/o\nn15\ndR17/q\ndR16/p\nn9\ndR0/l\nn9\ndR1/l\nn9\nn2\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
