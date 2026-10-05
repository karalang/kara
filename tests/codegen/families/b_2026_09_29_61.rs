//! B-2026-09-29-61 -- a `let` that shadows a by-value parameter's name loses
//! the caller's `Drop` body for that parameter.

use super::*;

/// B-2026-09-29-61, arm cell — `fn a3(s: Option[S]) -> i64 {{ match s {{ Some(x)
/// => {{ let s = 5; x.r.id + s }} None => 0 }} }}` over a fresh `Some(..)` ran no
/// `d1` compiled: the escape walk counted the shadowing `s` as a use of the
/// param, so the caller declined the temp's walk. The seeded walk now stops
/// counting a param's name for the rest of a block that shadows it. `a4` (the
/// same body with the local renamed) is the control.
#[test]
fn e2e_arm_let_shadowing_an_option_param_keeps_its_body() {
    let src = r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mks(i: i64) -> S { S { r: R { id: i }, s: f"ssssssssssssssssssssssssssss{i}" } }
fn a3(s: Option[S]) -> i64 { match s { Some(x) => { let s = 5; x.r.id + s } None => 0 } }
fn a4(s: Option[S]) -> i64 { match s { Some(x) => { let t = 5; x.r.id + t } None => 0 } }
fn main() {
    println(f"k{a3(Some(mks(1)))}"); let b = Some(mks(2)); println(f"k{a3(b)}")
    println(f"k{a4(Some(mks(3)))}"); let c = Some(mks(4)); println(f"k{a4(c)}")
    println("end")
}
"#;
    let want = "d1\nk6\nk7\nd2\nd3\nk8\nk9\nd4\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// B-2026-09-29-61 — the row's top-level cells: a `let` that shadows a by-value
/// param (`fn b4(s: S) -> i64 {{ let s = 5; s }}`, `fn k8(x: S) -> S {{ let x =
/// mks(28); x }}`) read as returning or escaping it, so its body ran on no
/// surface. With the drop schedule's per-param fate on by default, a param
/// whose every exit keeps it runs its body in the caller at the end of the call
/// (design.md rule 3), named or temporary, struct or `Option`.
#[test]
fn e2e_top_level_let_shadowing_a_param_keeps_its_body() {
    let src = r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mks(i: i64) -> S { S { r: R { id: i }, s: f"ssssssssssssssssssssssssssss{i}" } }
fn b4(s: S) -> i64 { let s = 5; s }
fn c1(s: S) -> i64 { let s = s.r.id; s }
fn c2(s: S) -> i64 { let t = 5; let s = t; s }
fn b5(s: Option[S]) -> i64 { let s = 5; s }
fn k8(x: S) -> S { let x = mks(28); x }
fn k7(x: Option[S]) -> Option[S] { let x = Some(mks(27)); x }
fn main() {
    println(f"k{b4(mks(1))}"); let a = mks(2); println(f"k{b4(a)}")
    println(f"k{c1(mks(3))}"); println(f"k{c2(mks(4))}")
    println(f"k{b5(Some(mks(5)))}"); let b = Some(mks(6)); println(f"k{b5(b)}")
    let d = mks(7); let i = k8(d); println(f"i{i.r.id}")
    let e = mks(8); k8(e); println("e")
    let j = k8(mks(9)); println(f"j{j.r.id}")
    let f = Some(mks(10)); let g = k7(f); println(f"g{g.is_some()}")
    println("end")
}
"#;
    let want = "d1\nk5\nk5\nd2\nd3\nk3\nd4\nk5\nd5\nk5\nk5\nd6\nd7\ni28\nd28\nd28\nd8\ne\nd9\nj28\nd28\nd10\ngtrue\nd27\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    // B-2026-10-05-9 — compiled, `c1(mks(3))` used to run no `d3`: codegen's
    // temporary walk asked the name-matching `fn_returns_param`, which read the
    // shadowing `let s = s.r.id; s` as handing the param back.
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
