//! B-2026-10-03-28 -- a `Result[Option[R], E]` by-value param of a
//! non-generic function, where `R` runs a user `Drop`, read freed memory
//! after the call.

use super::*;

/// B-2026-10-03-28 — the callee's `NestedBoxedEnumDrop` freed the inner box
/// before returning, and the caller's payload-bodies walk (the argument
/// temp's, or a named local's own let-site walk) read it afterwards. The
/// callee now runs the bodies ahead of its box drop and both caller walks
/// stand down. Covers a temp, a named local, `Ok(None)`, `Err`, a loop and a
/// conditional call.
///
/// The named-local lines print the body BEFORE the callee's result, where the
/// interpreter prints it after: PREDICTS B-2026-10-03-7, whose fix flips them.
#[test]
fn asan_result_option_param_bodies_read_no_freed_box() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn rr(x: Result[Option[R], i64]) -> i64 {
    match x {
        Ok(Some(w)) => w.id,
        Ok(None) => 2,
        Err(e) => e,
    }
}
fn main() {
    println(rr(Ok(Some(mk(1)))));
    let a: Result[Option[R], i64] = Ok(Some(mk(3)));
    println(rr(a));
    let n: Result[Option[R], i64] = Ok(None);
    println(rr(n));
    let e: Result[Option[R], i64] = Err(4);
    println(rr(e));
    let mut i = 0;
    while i < 2 { println(rr(Ok(Some(mk(10 + i))))); i = i + 1; }
    let d: Result[Option[R], i64] = Ok(Some(mk(20)));
    let k = 1;
    if k > 0 { println(rr(d)) }
    println("end");
}
"#,
        &[
            "dR1", "1", "dR3", "3", "2", "4", "dR10", "10", "dR11", "11", "dR20", "20", "end",
        ],
        "result_option_param_bodies",
    );
}
