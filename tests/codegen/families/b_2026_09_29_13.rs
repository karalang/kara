//! B-2026-09-29-13 — a binding that SHADOWS an `Option` / `Result` name with
//! a value of another type prints as the new value on the compiled backends,
//! not through the old binding's `Some(..)` / `None` renderer.

use super::*;

/// B-2026-09-29-13 — `fn s2(t: Option[i64]) { let t = 3; println(f"s2 {t}") }`
/// printed `s2 None` compiled (interp `s2 3`): the Display records for
/// `Option` / `Result` bindings are keyed by NAME and a rebind at another type
/// did not forget them. Neighbours: an inner-block shadow (and the outer name
/// rendering again after it), a `Result` param, same-type and cross-kind
/// rebinds, and `match` / `if let` / `let .. else` arm bindings that shadow
/// the scrutinee's name, including a nested `Option` payload.
#[test]
fn e2e_shadowed_option_binding_prints_as_its_new_type() {
    let src = r#"struct P { x: i64 }
struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: "ab".to_string() + "cd" } }
fn s2(t: Option[i64]) { let t = 3; println(f"s2 {t}") }
fn s3(t: Option[P]) { let t = 3; println(f"s3 {t}") }
fn s5(t: Option[P]) { let t = 3; println(t) }
fn s8() { let t: Option[i64] = None; { let t = 4; println(f"s8 {t}") } println(f"s8b {t}") }
fn r1(t: Result[i64, String]) { let t = 5; println(f"r1 {t}") }
fn o1(t: Option[i64]) { let t = t; println(f"o1 {t}") }
fn o2(t: Option[i64]) { let t = "x".to_string(); println(f"o2 {t}") }
fn o4(t: Option[i64]) { let t: Result[i64, String] = Ok(4); println(f"o4 {t}") }
fn o5(t: Option[i64]) { match t { Some(t) => println(f"o5 {t}"), None => println("o5n") } println(f"o5b {t}") }
fn o6(t: Option[i64]) { if let Some(t) = t { println(f"o6 {t}") } println(f"o6b {t}") }
fn o9() { let t: Result[i64, String] = Err("e".to_string()); { let t = 1; println(f"o9 {t}") } println(f"o9b {t}") }
fn o10(t: Option[i64]) -> i64 { let Some(t) = t else { return 0 }; println(f"o10 {t}"); t }
fn n1(t: Option[Option[i64]]) { match t { Some(t) => println(f"n1 {t}"), None => println("n1n") } }
fn h1(t: Option[S]) { let t = 3; println(f"sh{t}") }
fn main() {
    s2(Option.Some(7)); s3(Some(P { x: 1 })); s5(None); s8(); r1(Ok(1)); o1(Some(3)); o2(Some(4)); o4(None);
    o5(Some(6)); o6(Some(7)); o9(); println(f"{o10(Some(11))}"); n1(Some(Some(4))); n1(Some(None));
    h1(Some(mks(1))); println("end")
}"#;
    let want = "s2 3\ns3 3\n3\ns8 4\ns8b None\nr1 5\no1 Some(3)\no2 x\no4 Ok(4)\no5 6\no5b Some(6)\no6 7\no6b Some(7)\no9 1\no9b Err(e)\no10 11\n11\nn1 Some(4)\nn1 None\nsh3\ndS1\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
