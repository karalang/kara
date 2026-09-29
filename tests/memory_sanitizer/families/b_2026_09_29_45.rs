//! B-2026-09-29-45 -- a fresh `Option` / `Result` temp lent to a `ref` param
//! frees its box and runs its payload's `Drop` body once.

use super::*;

/// B-2026-09-29-45 — the leak half: a boxed payload's box (and the payload's
/// `String`) was freed by nobody, 33 B per call at `-O0`.
#[test]
fn asan_fresh_optres_temp_lent_to_ref_param_is_freed_once() {
    assert_clean_asan_run(
        r#"struct R { name: String, id: i64 }
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
"#,
        &[
            "dR6/f-heap-string-longer-than-sso",
            "n6",
            "dR7/g",
            "n2",
            "dR8/h",
            "dR9/r-heap-string-longer-than-sso",
            "n3",
            "dR10/j",
            "n3",
            "dS11",
            "n4",
            "dS12",
            "n4",
            "dR13/m",
            "n7",
            "dR14/n",
            "n8",
            "dR15/o",
            "n15",
            "dR17/q",
            "dR16/p",
            "n9",
            "dR0/l",
            "n9",
            "dR1/l",
            "n9",
            "n2",
            "end",
        ],
        "B-2026-09-29-45 fresh Option temp lent to a ref param",
    );
}
