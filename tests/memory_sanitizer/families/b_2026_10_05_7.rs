//! B-2026-10-05-7 — a binding the block does not declare, passed by value to a callee that keeps
//! it on no exit, runs its `Drop` body at the call, design.md § Drop ordering rule 3.

use super::*;

/// `match o { Some(r) => { eat(r); println("a") } .. }` printed `a dR1` on
/// every surface, where a `let` local passed the same way prints its body
/// first. Covers `Option`, user-enum and `Result` arms, `if let`, a struct
/// destructure, `let k = eat(r)`, a hand-off on one inner branch (both
/// paths), an enclosing local in a nested `if` (both paths) and a bare block,
/// and an arm inside a loop, whose move bit is re-armed each iteration.
#[test]
fn asan_binding_handed_to_keeping_callee_dies_at_the_call() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
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
"#,
        &[
            "dR1", "a", "dR2", "b", "dR3", "c", "dR4", "d4", "dR5", "e", "dR6", "f1", "dR7", "g",
            "dR7", "g", "dR8", "h", "h2", "dR8", "h2", "dR9", "i", "dR10", "j0", "dR11", "j1",
            "end",
        ],
        "binding_handed_to_keeping_callee",
    );
}
