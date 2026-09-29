//! B-2026-09-26-60 -- a named `Drop` local handed to a callee that keeps it
//! or hands it back, from a branch that does not run, still runs its body.

use super::*;

/// B-2026-09-26-60 — the call-site retraction of the binding's own `Drop`
/// wrapper was static, so a call compiled in a branch that did not run left
/// the value with no body on every compiled surface: memory-balanced for a
/// struct the callee entry-copies (`D`), and leaked as well for one it
/// forwards (`F`, which holds a `shared` field). Covered: a storing callee, a
/// hand-back callee, a generic callee, a method, an `if` expression, a `match`
/// arm, an `else` branch, a loop body, and a taken arm of each shape.
#[test]
fn e2e_keeping_callee_in_untaken_arm_runs_body() {
    let src = r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}") } }
fn mkd(n: i64) -> D { D { id: n, name: f"{n}-heap-string-longer-than-sso" } }
shared struct S { k: i64 }
struct F { id: i64, s: S, name: String }
impl Drop for F { fn drop(mut ref self) { println(f"dF{self.id}") } }
fn mkf(n: i64) -> F { F { id: n, s: S { k: n }, name: f"{n}-heap-string-longer-than-sso" } }
fn std(v: mut ref Vec[D], x: D) { v.push(x); }
fn stf(v: mut ref Vec[F], x: F) { v.push(x); }
fn keepd(x: D) -> D { x }
fn keepf(x: F) -> F { x }
fn keepg[T](x: T) -> T { x }
struct H { k: i64 }
impl H {
    fn hd(ref self, x: D) -> D { x }
    fn hf(ref self, x: F) -> F { x }
}
fn main() {
    let h = H { k: 1 };
    let mut v: Vec[D] = [];
    let mut w: Vec[F] = [];
    let c = false;
    { let d = mkd(1); if d.id > 3 { std(mut v, d); } println("a1") }
    { let d = mkd(2); if d.id > 3 { let k = keepd(d); println(f"k{k.id}"); } println("a2") }
    { let d = mkd(3); if d.id > 3 { let k = keepg(d); println(f"k{k.id}"); } else { println("e3") } println("a3") }
    { let d = mkd(4); if d.id > 9 { let k = h.hd(d); println(f"k{k.id}"); } println("a4") }
    { let d = mkd(5); let p = if c { keepd(d) } else { mkd(50) }; println(f"p{p.id}") }
    { let d = mkd(6); let p = match h.k { 2 => keepd(d), _ => mkd(60) }; println(f"p{p.id}") }
    for i in 7..10 { let d = mkd(i); if i == 8 { std(mut v, d); } }
    println("a5")
    { let d = mkd(11); if d.id > 3 { let k = keepd(d); println(f"k{k.id}"); } println("a6") }
    { let f = mkf(21); if f.id > 30 { stf(mut w, f); } println("b1") }
    { let f = mkf(22); if f.id > 30 { let k = keepf(f); println(f"k{k.id}"); } println("b2") }
    { let f = mkf(23); if f.id > 30 { let k = keepg(f); println(f"k{k.id}"); } println("b3") }
    { let f = mkf(24); if f.id > 30 { let k = h.hf(f); println(f"k{k.id}"); } println("b4") }
    { let f = mkf(25); let p = if c { keepf(f) } else { mkf(250) }; println(f"p{p.id}") }
    { let f = mkf(26); if f.id > 3 { let k = keepf(f); println(f"k{k.id}"); } println("b5") }
    println(f"end{v.len()}{w.len()}")
}
"#;
    let want = "dD1\na1\ndD2\na2\ne3\ndD3\na3\ndD4\na4\ndD5\np50\ndD50\ndD6\np60\ndD60\ndD7\ndD9\na5\nk11\ndD11\na6\ndF21\nb1\ndF22\nb2\ndF23\nb3\ndF24\nb4\ndF25\np250\ndF250\nk26\ndF26\nb5\nend10\ndD8\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
