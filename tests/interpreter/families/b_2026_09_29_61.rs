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
fn interp_arm_let_shadowing_an_option_param_keeps_its_body() {
    let out = run(r#"struct R { id: i64 }
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
"#);
    assert_eq!(out, "d1\nk6\nk7\nd2\nd3\nk8\nk9\nd4\nend\n");
}
