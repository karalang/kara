//! B-2026-09-29-51 -- a `let` that SHADOWS a parameter by name binds a local
//! that owns its value: a pattern binding out of it runs the body once.

use super::*;

/// B-2026-09-29-51 — `fn h5(x: Option[R]) -> i64 { let x = Some(mk(11));
/// match x { Some(r) => r.id, None => 0 } }` ran no body for the local's payload
/// on any compiled surface: the arm read `x` as the parameter. `if let`, a plain
/// struct param, and a binding with no pattern are the neighbours.
#[test]
fn e2e_local_shadowing_an_option_param_runs_its_payload_body() {
    let src = r#"struct R { name: String, id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.name}") } }
fn mk(n: i64, s: String) -> R { R { name: s, id: n } }
fn h5(x: Option[R]) -> i64 { let x = Some(mk(11, f"k")); match x { Some(r) => r.id, None => 0 } }
fn h6(x: Option[R]) -> i64 { let x = Some(mk(12, f"l")); 1 }
fn h7(x: Option[R]) -> i64 { let x = Some(mk(13, f"m")); if let Some(r) = x { return r.id } 0 }
fn h8(x: R) -> i64 { let x = mk(14, f"n"); x.id }
fn main() {
    println(f"n{h5(Some(mk(5, f"e")))}")
    println(f"n{h6(Some(mk(6, f"f")))}")
    println(f"n{h7(Some(mk(7, f"g")))}")
    println(f"n{h8(mk(8, f"h"))}")
    println("end")
}
"#;
    let want =
        "dR11/k\ndR5/e\nn11\ndR12/l\ndR6/f\nn1\ndR13/m\ndR7/g\nn13\ndR14/n\ndR8/h\nn14\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// B-2026-09-29-51 — the shadow is SCOPED: after an inner block's `let x`, the
/// name is the parameter again (the interpreter ran its body twice there,
/// once in the callee). Also a rebind through the param (`let x = x`,
/// `let x = pass(x)`), a shadowed `mut ref` param, and a shadow returned whole.
/// That last one (`k7`) also runs the shadowed param's `dR8/w` in the caller
/// since B-2026-09-29-61 (design.md rule 3); before it, no surface ran it.
#[test]
fn e2e_param_shadow_ends_with_its_block() {
    let src = r#"struct R { name: String, id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.name}") } }
fn mk(n: i64, s: String) -> R { R { name: s, id: n } }
fn k1(x: Option[R]) -> i64 { if true { let x = Some(mk(21, f"a")); match x { Some(r) => println(f"in{r.id}"), None => {} } } match x { Some(r) => r.id, None => 0 } }
fn k2(x: Option[R]) -> i64 { let x = x; match x { Some(r) => r.id, None => 0 } }
fn pass(o: Option[R]) -> Option[R] { o }
fn k3(x: Option[R]) -> i64 { let x = pass(x); match x { Some(r) => r.id, None => 0 } }
fn k4(x: R) -> i64 { let x = mk(24, f"d"); let y = x; y.id }
fn k5(x: Option[R], c: bool) -> i64 { if c { let x = Some(mk(25, f"e")); if let Some(r) = x { return r.id } } match x { Some(r) => r.id, None => 0 } }
fn k6(x: mut ref Option[R]) -> i64 { let x = Some(mk(26, f"f")); match x { Some(r) => r.id, None => 0 } }
fn k7(x: Option[R]) -> Option[R] { let x = Some(mk(27, f"g")); x }
fn main() {
    println(f"n{k1(Some(mk(1, f"p")))}")
    println(f"n{k2(Some(mk(2, f"q")))}")
    println(f"n{k3(Some(mk(3, f"r")))}")
    println(f"n{k4(mk(4, f"s"))}")
    println(f"n{k5(Some(mk(5, f"t")), true)}")
    println(f"n{k5(Some(mk(6, f"u")), false)}")
    let mut m = Some(mk(7, f"v"));
    println(f"n{k6(mut m)}")
    let g = k7(Some(mk(8, f"w")));
    println(f"g{g.is_some()}")
    println("end")
}
"#;
    let want = "in21\ndR21/a\ndR1/p\nn1\ndR2/q\nn2\ndR3/r\nn3\ndR24/d\ndR4/s\nn24\ndR25/e\ndR5/t\nn25\ndR6/u\nn6\ndR26/f\nn26\ndR7/v\ndR8/w\ngtrue\ndR27/g\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// B-2026-09-29-51 — the `ref` and owned param spellings side by side with a
/// fresh name, which was always right.
#[test]
fn e2e_shadowing_a_ref_param_and_an_owned_param() {
    let src = r#"struct R { name: String, id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.name}") } }
fn mk(n: i64, s: String) -> R { R { name: s, id: n } }
fn h1(x: ref R) -> i64 { let x = mk(7, f"g"); x.id }
fn h2(x: ref Option[R]) -> i64 { let x = Some(mk(8, f"h")); 1 }
fn h3(x: ref Option[R]) -> i64 { let y = Some(mk(9, f"i")); match y { Some(r) => r.id, None => 0 } }
fn h4(x: ref Option[R]) -> i64 { let x = Some(mk(10, f"j")); match x { Some(r) => r.id, None => 0 } }
fn h5(x: Option[R]) -> i64 { let x = Some(mk(11, f"k")); match x { Some(r) => r.id, None => 0 } }
fn main() {
    let a = mk(1, f"a"); println(f"n{h1(a)}"); println("a1");
    let b = Some(mk(2, f"b")); println(f"n{h2(b)}"); println("a2");
    let c = Some(mk(3, f"c")); println(f"n{h3(c)}"); println("a3");
    let d = Some(mk(4, f"d")); println(f"n{h4(d)}"); println("a4");
    println(f"n{h5(Some(mk(5, f"e")))}"); println("a5");
    println("end")
}
"#;
    let want = "dR7/g\nn7\ndR1/a\na1\ndR8/h\nn1\ndR2/b\na2\ndR9/i\nn9\ndR3/c\na3\ndR10/j\nn10\ndR4/d\na4\ndR11/k\ndR5/e\nn11\na5\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
