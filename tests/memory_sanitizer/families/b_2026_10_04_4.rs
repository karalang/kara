//! B-2026-10-04-4 — a by-value nested envelope param taken apart in two steps
//! (an envelope binding first, then the leaf out of it) runs the leaf's body
//! once and frees the box once.

use super::*;

/// `Some(o)` binds a view of the param's payload; a taking pattern over `o`
/// (`match o`, `if let`, `let ... else`) moves the leaf on. The callee's
/// payload walk stayed armed through the second step, so R's `Drop` body ran
/// at the call's end and again when `v` dropped, and the `let ... else`
/// spellings zeroed the box pointer as if `o` had carried it out (32 B lost,
/// 61 B when the leaf was only read).
#[test]
fn asan_two_step_nested_envelope_param_leaf_owned_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn m(x: Option[Option[R]], v: mut ref Vec[R]) -> i64 { match x { Some(o) => match o { Some(w) => { v.push(w); 1 } None => 2 }, None => 0 } }
fn i(x: Option[Option[R]], v: mut ref Vec[R]) -> i64 { if let Some(o) = x { if let Some(w) = o { v.push(w); return 1 } return 2 } 0 }
fn lm(x: Option[Option[R]], v: mut ref Vec[R]) -> i64 { let Some(o) = x else { return 0 }; match o { Some(w) => { v.push(w); 1 } None => 2 } }
fn ll(x: Option[Option[R]], v: mut ref Vec[R]) -> i64 { let Some(o) = x else { return 0 }; let Some(w) = o else { return 2 }; v.push(w); return 1 }
fn lr(x: Result[Option[R], i64], v: mut ref Vec[R]) -> i64 { let Ok(o) = x else { return 0 }; let Some(w) = o else { return 2 }; v.push(w); return 1 }
fn rd(x: Option[Option[R]]) -> i64 { let Some(o) = x else { return 0 }; let Some(w) = o else { return 2 }; return w.id }
fn main() {
    let mut v: Vec[R] = Vec.new();
    println(m(Some(Some(mk(1))), mut v));
    println(i(Some(Some(mk(2))), mut v));
    println(lm(Some(Some(mk(3))), mut v));
    println(ll(Some(Some(mk(4))), mut v));
    println(lr(Ok(Some(mk(5))), mut v));
    println(ll(Some(None), mut v));
    println(rd(Some(Some(mk(6)))));
    println(v.len());
    println("end");
}
"#,
        &[
            "1", "1", "1", "1", "1", "2", "dR6", "6", "5", "dR1", "dR2", "dR3", "dR4", "dR5", "end",
        ],
        "two_step_nested_envelope_param",
    );
}
