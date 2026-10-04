//! B-2026-10-03-53: a read-only envelope binding keeps the payload walk armed in the interpreter

use super::*;

/// B-2026-10-03-53: `Ok(o) => 1` / `if let Ok(o) = c` over a local
/// `Result[Option[R], E]`, and `Some(o)` over `Option[Option[R]]`, disarmed
/// the scrutinee's payload walk for a binding that registers no body, so R's
/// `Drop` body ran nowhere. Same output as both compiled backends.
#[test]
fn interp_read_only_envelope_binding_keeps_leaf_body() {
    let out = run(r#"struct R { id: i64, s: String }
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
"#);
    assert_eq!(out, "dR1\n1\n2\ndR2\ndR3\n3\n1\ndR4\ntrue\ndR5\nend\n");
}
