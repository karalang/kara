//! B-2026-10-03-53 — a whole-value binding of a nested `Option`/`Result`
//! envelope that the arm only reads keeps the place's walk armed.

use super::*;

/// An `Ok(o)` / `Some(o)` arm over a local `Result[Option[R], E]` or
/// `Option[Option[R]]` that never moves `o` ran R's `Drop` body on no surface,
/// memory balanced: the payload walk was disarmed for a binding that registers
/// no body of its own. A consuming arm (`v.push(o)`, `o` returned out of the
/// match) must still hand the body over, once.
#[test]
fn asan_read_only_envelope_binding_keeps_leaf_body() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn main() {
    let a: Result[Option[R], i64] = Ok(Some(mk(1)));
    let k = match a { Ok(o) => 1, Err(e) => e };
    println(k);
    let c: Result[Option[R], i64] = Ok(Some(mk(2)));
    if let Ok(o) = c { println(2) }
    let d: Option[Option[R]] = Some(Some(mk(3)));
    let j = match d { Some(o) => if o.is_some() { 3 } else { 0 }, None => 0 };
    println(j);
    let e: Result[Option[R], i64] = Ok(Some(mk(4)));
    let mut v: Vec[Option[R]] = Vec.new();
    match e { Ok(o) => v.push(o), Err(x) => println(x) }
    println(v.len());
    let f: Option[Option[R]] = Some(Some(mk(5)));
    let b: Option[R] = match f { Some(o) => o, None => None };
    println(b.is_some());
    println("end");
}
"#,
        &[
            "dR1", "1", "2", "dR2", "dR3", "3", "1", "dR4", "true", "dR5", "end",
        ],
        "read_only_envelope_binding",
    );
}
