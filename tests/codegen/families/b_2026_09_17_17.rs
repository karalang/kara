//! B-2026-09-17-17 -- a whole TUPLE payload bound by an `Option`/`Result` arm
//! and handed to a by-value free-fn param (`Some(t) => eat(t)`) runs its
//! element `Drop` bodies once, at the call (B-2026-10-05-7; at the arm's end before it).

use super::*;

/// B-2026-09-17-17 — `match o { Some(t) => eat(t) }` over `Option[(R, R)]`
/// printed no body on any surface: the read-through classifier counts `eat(t)`
/// as a use, so the place's payload walk was disarmed, and a tuple payload has
/// no type name for the arm binding to be funded by. The taking binding now
/// carries the element walk. By-value free-fn params are caller-retains, so
/// the bodies fire once, after the callee returns. Covers block and expression
/// arms, `if let`, `while let`, a param scrutinee, `Result`, a hand-back into a
/// local, two calls over one binding, and a loop.
#[test]
fn e2e_taken_tuple_payload_handed_to_by_value_param_runs_bodies() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}" } }
fn mko(i: i64) -> Option[(R, R)] { return Some((mk(i), mk(i + 1))) }
fn eat(t: (R, R)) { println(f"  e{t.0.id}") }
fn eat7(t: (R, R)) -> i64 { println("  eat"); return 7 }
fn keep(t: (R, R)) -> (R, R) { return t }
fn viaparam(o: Option[(R, R)]) { match o { Some(t) => { eat(t); println("  p") } None => {} } }
fn main() {
    println("row");    { let o = mko(1); println("  x"); match o { Some(t) => { println(f"  hit{eat7(t)}") } None => { println("  n") } } }
    println("block");  { let o = mko(3); match o { Some(t) => { eat(t) } None => {} } println("  after") }
    println("expr");   { let o = mko(5); match o { Some(t) => eat(t), None => {} } println("  after") }
    println("iflet");  { let o = mko(7); if let Some(t) = o { eat(t); println("  i") } println("  after") }
    println("whilet"); { let mut o = mko(9); while let Some(t) = o { eat(t); o = None; println("  w") } println("  after") }
    println("param");  viaparam(mko(11)); println("  after");
    println("result"); { let r: Result[(R, R), i64] = Ok((mk(13), mk(14))); match r { Ok(t) => { eat(t); println("  r") } Err(e) => {} } println("  after") }
    println("keep");   { let o = mko(15); match o { Some(t) => { let k = keep(t); println(f"  k{k.0.id}") } None => {} } println("  after") }
    println("twice");  { let o = mko(17); match o { Some(t) => { eat(t); eat(t); println("  2") } None => {} } println("  after") }
    println("loop");   for i in 0..2 { let o = mko(20 + i * 2); match o { Some(t) => { eat(t) } None => {} } }
    println("end");
}
"#,
    ) else {
        return;
    };
    let got: Vec<&str> = out.lines().collect();
    assert_eq!(
        got,
        [
            "row", "  x", "  eat", "  hit7", "  dR1", "  dR2", "block", "  e3", "  dR3", "  dR4",
            "  after", "expr", "  e5", "  dR5", "  dR6", "  after", "iflet", "  e7", "  dR7",
            "  dR8", "  i", "  after", "whilet", "  e9", "  dR9", "  dR10", "  w", "  after",
            "param", "  e11", "  p", "  dR11", "  dR12", "  after", "result", "  e13", "  dR13",
            "  dR14", "  r", "  after", "keep", "  k15", "  dR15", "  dR16", "  after", "twice",
            "  e17", "  e17", "  dR17", "  dR18", "  2", "  after", "loop", "  e20", "  dR20",
            "  dR21", "  e22", "  dR22", "  dR23", "end",
        ],
        "got:\n{out}"
    );
}
