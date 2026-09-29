//! B-2026-09-29-42 — a field binding of a destructured by-value `Option` /
//! `Result` param payload rebound inside the callee (`Some(S { r, s }) =>
//! { let y = r; y.id }`). B-2026-09-29-2 resolved a projection off the FIELD
//! binding from the field's type; one rebind further on, `y.id` was resolved
//! from the payload's type again, named no field of `S`, scored as an escape,
//! and a fresh temp's `Drop` body ran on no compiled surface.

use super::*;

/// B-2026-09-29-42 — `match`, `if let`, `while let`, a chained rebind, a
/// renamed field binding, a `Result`, a rebind read and then moved out, and a
/// conditional move, each with a fresh temp and a named argument. Before the
/// fix every fresh-temp body was lost at `-O0` and `-O2`.
#[test]
fn asan_optres_param_field_binding_rebind_runs_temp_body_at_caller() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mks(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn keep(x: R) { println(f"kp{x.id}") }
fn f1(a: Option[S]) -> i64 { match a { Some(S { r, s }) => { let y = r; y.id } None => 0 } }
fn f2(a: Option[S]) -> i64 { if let Some(S { r, s }) = a { let y = r; y.id } else { 0 } }
fn f3(a: Option[S]) -> i64 { match a { Some(S { r, s }) => { let y = r; let z = y; z.id } None => 0 } }
fn f4(a: Option[S]) -> i64 { match a { Some(S { r, s }) => { let y = r; let n = y.id; keep(y); n } None => 0 } }
fn f5(a: Option[S]) -> i64 { match a { Some(S { r: q, s }) => { let y = q; y.id } None => 0 } }
fn f6(a: Result[S, i64]) -> i64 { match a { Ok(S { r, s }) => { let y = r; y.id } Err(e) => e } }
fn f7(a: Option[S]) -> i64 { match a { Some(S { r, s }) => { let y = r; if y.id > 13 { keep(y); return 5 } y.id } None => 0 } }
fn f8(a: Option[S]) -> i64 { let mut t = 0; while let Some(S { r, s }) = a { let y = r; t = y.id; break; } t }
fn main() {
    println(f"a{f1(Some(mks(1)))}"); let x1 = Some(mks(2)); println(f"b{f1(x1)}");
    println(f"a{f2(Some(mks(3)))}"); let x2 = Some(mks(4)); println(f"b{f2(x2)}");
    println(f"a{f3(Some(mks(5)))}"); let x3 = Some(mks(6)); println(f"b{f3(x3)}");
    println(f"a{f4(Some(mks(7)))}"); let x4 = Some(mks(8)); println(f"b{f4(x4)}");
    println(f"a{f5(Some(mks(9)))}"); let x5 = Some(mks(10)); println(f"b{f5(x5)}");
    println(f"a{f6(Ok(mks(11)))}"); let x6: Result[S, i64] = Ok(mks(12)); println(f"b{f6(x6)}");
    println(f"a{f7(Some(mks(13)))}"); let x7 = Some(mks(14)); println(f"b{f7(x7)}");
    println(f"a{f8(Some(mks(15)))}"); let x8 = Some(mks(16)); println(f"b{f8(x8)}");
    println("end")
}
"#,
        &[
            "d1", "a1", "b2", "d2", "d3", "a3", "b4", "d4", "d5", "a5", "b6", "d6", "kp7", "d7",
            "a7", "kp8", "b8", "d8", "d9", "a9", "b10", "d10", "d11", "a11", "b12", "d12", "d13",
            "a13", "kp14", "b5", "d14", "d15", "a15", "b16", "d16", "end",
        ],
        "asan_optres_param_field_binding_rebind_runs_temp_body_at_caller",
        20,
    );
}

/// B-2026-09-29-42 — the `let .. else` spelling.
#[test]
fn asan_optres_param_let_else_field_binding_rebind_runs_temp_body_at_caller() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mks(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn f(a: Option[S]) -> i64 { let Some(S { r, s }) = a else { return 0 }; let y = r; y.id }
fn main() { println(f"a{f(Some(mks(1)))}"); let x = Some(mks(2)); println(f"b{f(x)}"); println("end") }
"#,
        &["d1", "a1", "b2", "d2", "end"],
        "asan_optres_param_let_else_field_binding_rebind_runs_temp_body_at_caller",
        2,
    );
}
