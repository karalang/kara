//! B-2026-09-30-102 — an `Ok`/`Err` arm that destructures a struct payload and
//! binds only primitive fields (`Ok(H { n, .. })`, `Ok(H { v: _, n })`) zeroed the
//! source's payload words as if it had taken the heap. The `R` in `v` then
//! leaked, and the source's body walk ran on zeroed words (`d0` for `d1`).
//! Such an arm now takes nothing, generic payload or not.

use super::*;

/// B-2026-09-30-102 — `..` and `v: _` on both sides, a generic payload, a source living past the match, and the full destructure that was already right.
#[test]
fn e2e_result_struct_destructure_of_scalar_fields_takes_no_heap() {
    let src = r#"
struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct G[T] { v: T, n: i64 }
struct H { v: R, n: i64 }
fn mk(k: i64) -> R { R { id: k, s: f"a-heap-string-longer-than-sso-{k}" } }
fn c1() -> i64 { let o: Result[H, i64] = Result.Ok(H { v: mk(1), n: 10 }); match o { Result.Ok(H { n, .. }) => n, Result.Err(e) => e } }
fn c2() -> i64 { let o: Result[H, i64] = Result.Ok(H { v: mk(2), n: 20 }); match o { Result.Ok(H { v: _, n }) => n, Result.Err(e) => e } }
fn c3() -> i64 { let o: Result[i64, H] = Result.Err(H { v: mk(3), n: 30 }); match o { Result.Ok(k) => k, Result.Err(H { n, .. }) => n } }
fn c4() -> i64 { let o: Result[G[R], i64] = Result.Ok(G { v: mk(4), n: 40 }); match o { Result.Ok(G { n, .. }) => n, Result.Err(e) => e } }
fn c5() -> i64 { let o: Result[G[R], i64] = Result.Ok(G { v: mk(5), n: 50 }); match o { Result.Ok(G { v: _, n }) => n, Result.Err(e) => e } }
fn c6() -> i64 { let o: Result[H, i64] = Result.Ok(H { v: mk(6), n: 60 }); let k = match o { Result.Ok(H { n, .. }) => n, Result.Err(e) => e }; println("mid"); k }
fn c7() -> i64 { let o: Result[H, i64] = Result.Ok(H { v: mk(7), n: 70 }); match o { Result.Ok(H { v, n }) => v.id + n, Result.Err(e) => e } }
fn main() {
    println(c1()); println(c2()); println(c3()); println(c4()); println(c5()); println(c6()); println(c7());
    println("end")
}"#;
    let want = "d1\n10\nd2\n20\nd3\n30\nd4\n40\nd5\n50\nd6\nmid\n60\nd7\n77\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
