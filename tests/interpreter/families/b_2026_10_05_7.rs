//! B-2026-10-05-7 — a binding the block does not declare, passed by value to a callee that keeps
//! it on no exit, runs its `Drop` body at the call, design.md § Drop ordering rule 3.

use super::*;

/// The interpreter half of B-2026-10-05-7: a `match` / `if let` payload
/// binding, or an enclosing block's local used in a nested block, handed to
/// `eat` ran its body at the end of the arm or the enclosing statement. Same
/// program as the memory-sanitizer fixture, so the backends share one answer.
#[test]
fn interp_binding_handed_to_keeping_callee_dies_at_the_call() {
    let out = run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn eat(r: R) -> i64 { r.id }
struct S { r: R, n: i64 }
enum E { A(R), B }
fn opt() { let o = Some(mk(1)); match o { Some(r) => { eat(r); println("a") } None => {} } }
fn uen() { let o = E.A(mk(2)); match o { E.A(r) => { eat(r); println("b") } E.B => {} } }
fn iflet() { let o = Some(mk(3)); if let Some(r) = o { eat(r); println("c") } }
fn letk() { let o = Some(mk(4)); match o { Some(r) => { let k = eat(r); println(f"d{k}") } None => {} } }
fn res() { let o: Result[R, i64] = Ok(mk(5)); match o { Ok(r) => { eat(r); println("e") } Err(_) => {} } }
fn st() { let s = S { r: mk(6), n: 1 }; match s { S { r, n } => { eat(r); println(f"f{n}") } } }
fn branch(c: bool) { let o = Some(mk(7)); match o { Some(r) => { if c { eat(r); } println("g") } None => {} } }
fn nested(c: bool) { let r = mk(8); if c { eat(r); println("h") } println("h2") }
fn bare() { let r = mk(9); { eat(r); println("i") } }
fn inloop() { for i in 0..2 { let o = Some(mk(10 + i)); match o { Some(r) => { eat(r); println(f"j{i}") } None => {} } } }
fn main() {
    opt(); uen(); iflet(); letk(); res(); st();
    branch(true); branch(false); nested(true); nested(false); bare(); inloop();
    println("end");
}
"#);
    assert_eq!(out, "dR1\na\ndR2\nb\ndR3\nc\ndR4\nd4\ndR5\ne\ndR6\nf1\ndR7\ng\ndR7\ng\ndR8\nh\nh2\ndR8\nh2\ndR9\ni\ndR10\nj0\ndR11\nj1\nend\n");
}
