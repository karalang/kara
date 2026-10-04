//! B-2026-10-03-54 — a nested `match` that only reads the leaf of an
//! envelope binding leaves the leaf with the outer place.

use super::*;

/// `Ok(o) => match o { Some(i) => i.id, None => 0 }` over a
/// `Result[Option[R], E]` leaked R's heap compiled for a by-value param, and
/// for a local also lost R's `Drop` body: the outer arm counted `match o` as a
/// take of the envelope and retracted the leaf drop, and nothing under the
/// inner arm registered one. A guarded inner arm and the `if let` outer form
/// leaked the same way. The leak half is carried by the `-O0` leg (at `-O2`
/// the non-escaping allocation is scalarized away); the missing body shows at
/// both levels.
#[test]
fn asan_reading_inner_match_leaves_leaf_with_place() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn rr(x: Result[Option[R], i64]) -> i64 { match x { Ok(o) => match o { Some(i) => i.id, None => 0 }, Err(e) => e } }
fn ri(x: Result[Option[R], i64]) -> i64 { if let Ok(o) = x { match o { Some(i) => i.id, None => 0 } } else { 0 } }
fn main() {
    println(rr(Ok(Some(mk(1)))));
    println(ri(Ok(Some(mk(2)))));
    let a: Result[Option[R], i64] = Ok(Some(mk(3)));
    let k = match a { Ok(o) => match o { Some(i) => i.id, None => 0 }, Err(e) => e };
    println(k);
    let b: Result[Option[R], i64] = Ok(Some(mk(4)));
    let j = match b { Ok(o) => match o { Some(i) if i.id > 0 => i.id, _ => 0 }, Err(e) => e };
    println(j);
    println(rr(Err(5)));
    println("end");
}
"#,
        &["dR1", "1", "dR2", "2", "dR3", "3", "dR4", "4", "5", "end"],
        "reading_inner_match_leaf",
    );
}
