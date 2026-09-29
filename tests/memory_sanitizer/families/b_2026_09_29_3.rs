//! B-2026-09-29-3 — a whole immutable rebind of an `Option` param's
//! payload binding inside the callee (`Some(x) => { let y = x; y.r.id }`) is
//! a view of the caller's payload, so a copy read off the new name leaves the
//! payload's `Drop` body with the caller exactly as the same read off `x`
//! does (design.md rule 3).

use super::*;

/// B-2026-09-29-3 — `if let`, `match`, `let .. else`, a read before a later
/// statement, a `let` of the read, a two-step rebind and an identity hand-back,
/// each called with a fresh temp and a named argument. Before the fix every
/// compiled named call ran its body twice (once at the rebound local's end in
/// the callee, once at the caller) and every fresh temp ran it inside the
/// callee, ahead of the callee's later statements.
#[test]
fn asan_optres_param_payload_rebind_copy_read_runs_body_once_at_caller() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mks(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn id(a: Option[S]) -> Option[S] { a }
fn f1(a: Option[S]) -> i64 { if let Some(x) = a { let y = x; y.r.id } else { 0 } }
fn f2(a: Option[S]) -> i64 { match a { Some(x) => { let y = x; y.r.id } None => 0 } }
fn f3(a: Option[S]) -> i64 { let Some(x) = a else { return 0 }; let y = x; y.r.id }
fn f4(a: Option[S]) -> i64 { let r = match a { Some(x) => { let y = x; println("mid"); y.r.id } None => 0 }; println("post"); r }
fn f5(a: Option[S]) -> i64 { match a { Some(x) => { let y = x; let n = y.r.id; println("mid"); n } None => 0 } }
fn f6(a: Option[S]) -> i64 { match a { Some(x) => { let y = x; let z = y; z.r.id } None => 0 } }
fn f7(a: Option[S]) -> i64 { if let Some(x) = id(a) { let y = x; y.r.id } else { 0 } }
fn main() {
    println(f"k{f1(Some(mks(1)))}"); let a1 = Some(mks(2)); println(f"j{f1(a1)}");
    println(f"k{f2(Some(mks(3)))}"); let a2 = Some(mks(4)); println(f"j{f2(a2)}");
    println(f"k{f3(Some(mks(5)))}"); let a3 = Some(mks(6)); println(f"j{f3(a3)}");
    println(f"k{f4(Some(mks(7)))}"); let a4 = Some(mks(8)); println(f"j{f4(a4)}");
    println(f"k{f5(Some(mks(9)))}"); let a5 = Some(mks(10)); println(f"j{f5(a5)}");
    println(f"k{f6(Some(mks(11)))}"); let a6 = Some(mks(12)); println(f"j{f6(a6)}");
    println(f"k{f7(Some(mks(13)))}"); let a7 = Some(mks(14)); println(f"j{f7(a7)}");
    println("end")
}
"#,
        &[
            "d1", "k1", "j2", "d2", "d3", "k3", "j4", "d4", "d5", "k5", "j6", "d6", "mid", "post",
            "d7", "k7", "mid", "post", "j8", "d8", "mid", "d9", "k9", "mid", "j10", "d10", "d11",
            "k11", "j12", "d12", "d13", "k13", "j14", "d14", "end",
        ],
        "asan_optres_param_payload_rebind_copy_read_runs_body_once_at_caller",
        10,
    );
}
