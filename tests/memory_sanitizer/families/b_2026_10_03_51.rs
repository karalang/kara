//! B-2026-10-03-51 -- a nested `Result[Option[R], E]` box freed without its
//! leaf's fields when nothing bound the leaf, and a `let ... else` that moved
//! the leaf out of a local freed it twice.

use super::*;

/// B-2026-10-03-51 — a by-value param's nested box was registered box-only,
/// so a body that never bound the leaf (`Ok(_)`, `Ok(o) => 1`, an `Err(e)`
/// arm beside it, no match at all) left R's fields owned by nobody.
///
/// `-O0` only (the `asan-o0-leg.sh` leg): at the default `-O2` the values
/// are scalarized and the leak is not observable, so this passes on the
/// unfixed tree there.
#[test]
fn asan_nested_envelope_param_frees_unbound_leaf() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn wild(x: Result[Option[R], i64]) -> i64 { match x { Ok(_) => 1, Err(e) => e } }
fn env(x: Result[Option[R], i64]) -> i64 { match x { Ok(o) => 2, Err(e) => e } }
fn rd(x: Result[Option[R], i64]) -> i64 { match x { Ok(o) => if o.is_some() { 3 } else { 0 }, Err(e) => e } }
fn none(x: Result[Option[R], i64]) -> i64 { return 4 }
fn main() {
    println(wild(Ok(Some(mk(1)))));
    println(env(Ok(Some(mk(2)))));
    println(rd(Ok(Some(mk(3)))));
    println(none(Ok(Some(mk(4)))));
    println("end");
}
"#,
        &["dR1", "1", "dR2", "2", "dR3", "3", "dR4", "4", "end"],
        "nested_envelope_param_unbound_leaf",
    );
}

/// B-2026-10-03-51 — the local spellings: an `Ok(o)` arm, an `Ok(_)` arm
/// beside `Err(e)`, and an `if let` that only reads its envelope all retracted
/// the leaf drop, and a `let ... else` that moves the leaf into a `Vec`
/// freed it twice.
///
/// `dR1` and `dR3` were missing on every surface until B-2026-10-03-53: an
/// envelope binding `o` of a local ran no `Drop` body for its leaf.
#[test]
fn asan_nested_envelope_local_leaf_owned_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn main() {
    let a: Result[Option[R], i64] = Ok(Some(mk(1)));
    let k = match a { Ok(o) => 1, Err(e) => e };
    println(k);
    let b: Result[Option[R], i64] = Ok(Some(mk(2)));
    match b { Ok(_) => println(2), Err(e) => println(e) }
    let c: Result[Option[R], i64] = Ok(Some(mk(3)));
    if let Ok(o) = c { println(3) }
    let mut v: Vec[R] = Vec.new();
    let d: Result[Option[R], i64] = Ok(Some(mk(4)));
    let Ok(Some(w)) = d else { return };
    v.push(w);
    println(v.len());
    println("end");
}
"#,
        &["dR1", "1", "2", "dR2", "3", "dR3", "1", "dR4", "end"],
        "nested_envelope_local_leaf",
    );
}
