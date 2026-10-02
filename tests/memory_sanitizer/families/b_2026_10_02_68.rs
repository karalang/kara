//! B-2026-10-02-68 -- a fresh nested `Option[Option[R]]` temporary passed to a
//! GENERIC callee frees the leaf payload's heap.

use super::*;

/// B-2026-10-02-68 — `track_boxed_optres_arg_temp` freed the envelope chain
/// but armed an interior drop only for a TUPLE payload, so the user-struct
/// leaf at the bottom of `Option[Option[R]]` lost its `String` (29 B per
/// call); the non-generic leg A already drops the chain's leaf. Covers a
/// bound arm, a `_` arm, the `Option[T]` at `T = Option[R]` spelling, and the
/// tuple payload that was already right.
#[test]
fn asan_nested_option_temp_to_generic_callee_frees_leaf() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn cls[T](x: Option[Option[T]]) -> i64 {
    match x {
        Some(inner) => 7,
        None => 0,
    }
}
fn clsw[T](x: Option[Option[T]]) -> i64 {
    match x {
        Some(_) => 6,
        None => 0,
    }
}
fn bare[T](x: Option[T]) -> i64 {
    match x {
        Some(inner) => 5,
        None => 0,
    }
}
fn cnt[T](x: Option[(T, i64)]) -> i64 {
    match x {
        Some(t) => t.1,
        None => 0,
    }
}
fn main() {
    println(cls(Some(Some(mk(1)))));
    println(clsw(Some(Some(mk(2)))));
    println(bare(Some(Some(mk(3)))));
    println(cnt(Some((mk(4), 4))));
    println("end");
}
"#,
        &["dR1", "7", "dR2", "6", "dR3", "5", "dR4", "4", "end"],
        "nested_option_temp_to_generic_callee",
    );
}
