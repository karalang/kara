//! B-2026-09-27-104 — a call whose argument is an `if` / `match` that hands a
//! local's `Drop` field over on one arm, sitting anywhere but a statement's
//! top: an f-string part, a condition, `let _ =`, a struct literal field.

use super::*;

/// B-2026-09-27-104 — `show(if c { p.a } else { mk(9) })` NESTED inside an
/// f-string, an `if` condition, `let _ =`, a method call's f-string, a `match`
/// argument, a struct literal field, and the two-hop `x.w.r` spelling, runs
/// each body once on both legs. Only calls at a statement's top were seeded
/// for the conditional move, so the interpreter lost the minting arm's body
/// and compiled code ran the handed-over field's body a second time over its
/// freed name.
#[test]
fn e2e_nested_call_with_a_branch_argument_runs_each_body_once() {
    let src = r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"d{self.id}{self.name}") } }
fn mkd(n: i64) -> D { D { id: n, name: f"n{n}" } }
struct P { a: D, b: i64 }
struct W { r: D, s: D }
struct X { w: W, t: D }
struct Q { k: i64 }
impl Q { fn see(self, d: D) -> i64 { d.id } }
struct K { v: i64 }
fn show(d: D) -> i64 { d.id }
fn fstr(c: bool) { let p = P { a: mkd(10), b: 0 }; println(f"s{show(if c { p.a } else { mkd(19) })}"); }
fn cond(c: bool) { let p = P { a: mkd(20), b: 0 }; if show(if c { p.a } else { mkd(29) }) > 0 { println("pos") } }
fn wild(c: bool) { let p = P { a: mkd(30), b: 0 }; let _ = show(if c { p.a } else { mkd(39) }); println("w"); }
fn meth(c: bool) { let p = P { a: mkd(40), b: 0 }; let q = Q { k: 0 }; println(f"s{q.see(if c { p.a } else { mkd(49) })}"); }
fn mtch(c: bool) { let p = P { a: mkd(50), b: 0 }; println(f"s{show(match c { true => p.a, false => mkd(59) })}"); }
fn slit(c: bool) { let p = P { a: mkd(60), b: 0 }; let k = K { v: show(if c { p.a } else { mkd(69) }) }; println(f"k{k.v}"); }
fn deep(c: bool) { let x = X { w: W { r: mkd(70), s: mkd(71) }, t: mkd(72) }; println(f"s{show(if c { x.w.r } else { mkd(79) })}"); }
fn main() {
    fstr(false); fstr(true);
    cond(false); cond(true);
    wild(false); wild(true);
    meth(false); meth(true);
    mtch(false); mtch(true);
    slit(false); slit(true);
    deep(false); deep(true);
    println("end")
}"#;
    let want = "d19n19\ns19\nd10n10\nd10n10\ns10\nd29n29\npos\nd20n20\nd20n20\npos\nd39n39\nd30n30\nw\nd30n30\nw\nd49n49\ns49\nd40n40\nd40n40\ns40\nd59n59\ns59\nd50n50\nd50n50\ns50\nd69n69\nd60n60\nk69\nd60n60\nk60\nd79n79\ns79\nd72n72\nd71n71\nd70n70\nd70n70\ns70\nd72n72\nd71n71\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
