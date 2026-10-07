//! B-2026-10-02-73 -- a generic arm `Some(Some(w))` that only reads its leaf
//! leaked the inner box of a by-value `Option[Option[T]]` param.

use super::*;

/// B-2026-10-02-73 — the generic caller read a nested `Some(Some(w))` chain as
/// taking the payload unconditionally, so it declined the payload's drop while
/// the monomorph, whose arm only reads `w`, freed nothing. Covers a `match`
/// arm, a wildcard leaf, an `if let`, a guarded arm, a three-deep chain, and an
/// arm that does move the leaf out (which must stay the callee's).
#[test]
fn asan_nested_optres_chain_read_only_leaf_frees_the_box() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn deep[T](x: Option[Option[T]]) -> i64 {
    match x {
        Some(Some(w)) => 1,
        _ => 0,
    }
}
fn deepn[T](x: Option[Option[T]]) -> i64 {
    match x {
        Some(Some(_)) => 1,
        _ => 0,
    }
}
fn deepif[T](x: Option[Option[T]]) -> i64 {
    if let Some(Some(w)) = x { return 1 }
    return 0
}
fn deepg[T](x: Option[Option[T]]) -> i64 {
    match x {
        Some(Some(w)) if true => 1,
        Some(_) => 2,
        _ => 0,
    }
}
fn triple[T](x: Option[Option[Option[T]]]) -> i64 {
    match x {
        Some(Some(Some(w))) => 1,
        _ => 0,
    }
}
fn deepk[T](x: Option[Option[T]]) -> Option[T] {
    match x {
        Some(Some(w)) => Some(w),
        _ => None,
    }
}
fn main() {
    println(deep(Some(Some(mk(1)))));
    println(deepn(Some(Some(mk(2)))));
    println(deepif(Some(Some(mk(3)))));
    println(deepg(Some(Some(mk(4)))));
    println(triple(Some(Some(Some(mk(5))))));
    match deepk(Some(Some(mk(6)))) {
        Some(r) => println(r.id),
        None => println("none"),
    }
    println("end")
}
"#,
        &[
            "dR1", "1", "dR2", "1", "dR3", "1", "dR4", "1", "dR5", "1", "6", "dR6", "end",
        ],
        "nested_optres_chain_read_only_leaf",
    );
}
