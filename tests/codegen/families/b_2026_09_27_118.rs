//! B-2026-09-27-118 -- a field read on the result of a method called on an
//! enum-variant constructor temp (`E.X(r).take().id`) failed the build with
//! "cannot resolve field" while the named receiver (`g.take().id`) built.

use super::*;

/// The row's program and its neighbours: a multi-payload variant, a chained
/// struct field through a method returning a struct, a `ref self` method, and
/// a `shared enum` constructor. Each element body runs once, as under the
/// interpreter.
#[test]
fn e2e_field_read_on_method_result_over_enum_ctor_temp() {
    let src = r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct W { r: R, n: i64 }
enum E { X(R), Y, Z(i64, R) }
impl E {
    fn take(self) -> R { match self { E.X(t) => { return t; } E.Y => { return R { id: 0, tag: f"z" }; } E.Z(_, t) => { return t; } } }
    fn wrap(self) -> W { match self { E.X(t) => W { r: t, n: 1 }, _ => W { r: R { id: 9, tag: f"w" }, n: 2 } } }
    fn peek(ref self) -> i64 { match self { E.X(t) => t.id, _ => -1 } }
}
shared enum S { A(i64), B }
impl S { fn val(self) -> R { match self { S.A(n) => R { id: n, tag: f"s" }, S.B => R { id: 0, tag: f"b" } } } }
fn main() {
    println(f"a{E.X(R { id: 5, tag: f"e.." }).take().id}");
    println(f"b{E.Z(3, R { id: 6, tag: f"zz" }).take().tag}");
    println(f"d{E.X(R { id: 7, tag: f"w" }).wrap().r.id}");
    println(f"e{E.X(R { id: 8, tag: f"p" }).peek()}");
    println(f"f{S.A(4).val().id}");
    println("end");
}
"#;
    let want = "a5\ndR5\nbzz\ndR6\nd7\ndR7\ndR8\ne8\nf4\ndR4\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
