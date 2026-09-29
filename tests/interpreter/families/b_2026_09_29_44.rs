//! B-2026-09-29-44 -- an arm binding taken out of a BORROWED (`ref` /
//! `mut ref`) `Option`/`Result` param is a view: the caller owns the value
//! and runs its body, once.

use super::*;

/// B-2026-09-29-44 — `match`, `if let`, a wildcard arm, a `Result` param and
/// `let … else` over a `ref` param each run the payload's body once, in the
/// caller. The unfixed interpreter ran it in the borrower as well (twice).
#[test]
fn test_arm_binding_out_of_ref_optres_param_runs_body_once() {
    let out = run(r#"struct R { name: String, id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.name}") } }
fn mk(n: i64, s: String) -> R { R { name: s, id: n } }
fn g1(x: ref Option[R]) -> i64 { match x { Some(r) => r.id, None => 0 } }
fn g2(x: ref Option[R]) -> i64 { if let Some(r) = x { return r.id } 0 }
fn g3(x: ref Option[R]) -> i64 { match x { Some(_) => 1, None => 0 } }
fn g4(x: ref Result[R, i64]) -> i64 { match x { Ok(r) => r.id, Err(e) => e } }
fn g5(x: ref Option[R]) -> i64 { let Some(r) = x else { return 0 }; r.id }
fn main() {
    let a = Some(mk(1, f"a")); println(f"n{g1(a)}"); println("a1");
    let b = Some(mk(2, f"b")); println(f"n{g2(b)}"); println("a2");
    let c = Some(mk(3, f"c")); println(f"n{g3(c)}"); println("a3");
    let d: Result[R, i64] = Ok(mk(4, f"d")); println(f"n{g4(d)}"); println("a4");
    let e = Some(mk(5, f"e")); println(f"n{g5(e)}"); println("a5");
    println(f"n{g1(Some(mk(6, f"f")))}"); println("end")
}
"#);
    assert_eq!(
        out,
        "n1
dR1/a
a1
n2
dR2/b
a2
n1
dR3/c
a3
n4
dR4/d
a4
n5
dR5/e
a5
dR6/f
n6
end
",
        "got:
{out}"
    );
}

/// B-2026-09-29-44 — the `mut ref` spelling, a `ref self` receiver, and a
/// LOCAL that shadows a borrowed param by name. The shadowing local owns its
/// value, so its body still runs in the callee (`dR7`, `dR9`).
#[test]
fn test_borrowed_param_view_rule_respects_mut_ref_and_shadowing() {
    let out = run(r#"struct R { name: String, id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.name}") } }
fn mk(n: i64, s: String) -> R { R { name: s, id: n } }
struct H { o: Option[R] }
impl H { fn peek(ref self) -> i64 { match self.o { Some(r) => r.id, None => 0 } } }
fn g6(x: ref Option[R]) -> i64 { let x = Some(mk(7, f"g")); match x { Some(r) => r.id, None => 0 } }
fn g7(x: mut ref Option[R]) -> i64 { match x { Some(r) => r.id, None => 0 } }
fn g8(x: ref Option[R]) -> i64 { let x = Some(mk(9, f"i")); let Some(r) = x else { return 0 }; r.id }
fn g9(x: mut ref Option[R]) -> i64 { let Some(r) = x else { return 0 }; r.id }
fn main() {
    let a = Some(mk(1, f"a")); println(f"n{g6(a)}"); println("a6");
    let mut b = Some(mk(2, f"b")); println(f"n{g7(mut b)}"); println("a7");
    let c = Some(mk(3, f"c")); println(f"n{g8(c)}"); println("a8");
    let mut d = Some(mk(4, f"d")); println(f"n{g9(mut d)}"); println("a9");
    let h = H { o: Some(mk(5, f"h")) }; println(f"n{h.peek()}"); println("ah");
    println("end")
}
"#);
    assert_eq!(
        out,
        "dR7/g
n7
dR1/a
a6
n2
dR2/b
a7
dR9/i
n9
dR3/c
a8
n4
dR4/d
a9
n5
dR5/h
ah
end
",
        "got:
{out}"
    );
}
