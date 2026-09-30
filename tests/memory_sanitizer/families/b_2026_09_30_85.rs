//! B-2026-09-30-85 — a generic struct (`G[String]`) inline in a `Result`
//! payload was asked about by NAME, which resolves the erased layout: its
//! `v: T` owns nothing there. So the let-site payload drop was never
//! registered, and an arm binding over a disarmed source was refused an owner.
//! Both now ask the binding's instantiation.

use super::*;

/// B-2026-09-30-85 — `Ok` and `Err` sides, a nested `Vec`, a rebind, a reassignment, a tail match that reads, hands on and destructures its binding, a by-value param, and a fresh-temp scrutinee whose arm reads, moves a field out, or ignores its binding.
#[test]
fn asan_generic_struct_inline_result_payload_is_dropped_at_instantiation() {
    assert_clean_asan_run_min_allocs(
        r#"struct G[T] { v: T, n: i64 }
fn mk(k: i64) -> String { f"a-heap-string-longer-than-sso-{k}" }
fn mkr(k: i64) -> Result[G[String], i64] { Result.Ok(G { v: mk(k), n: k }) }
fn mke(k: i64) -> Result[i64, G[String]] { Result.Err(G { v: mk(k), n: k }) }
fn take(g: G[String]) -> i64 { g.v.len() }
fn eat(r: Result[G[String], i64]) -> i64 { match r { Result.Ok(g) => g.n, Result.Err(e) => e } }
fn c1() -> i64 { let r: Result[G[String], i64] = Result.Ok(G { v: mk(1), n: 10 }); 1 }
fn c2() -> i64 { let r: Result[i64, G[String]] = Result.Err(G { v: mk(2), n: 20 }); 2 }
fn c3() -> i64 { let r: Result[i64, G[Vec[String]]] = Result.Err(G { v: [mk(3), mk(33)], n: 30 }); 3 }
fn c4() -> i64 { let r: Result[G[String], i64] = Result.Ok(G { v: mk(4), n: 40 }); match r { Result.Ok(g) => g.v.len() + g.n, Result.Err(e) => e } }
fn c5() -> i64 { let r = mkr(5); let p = r; match p { Result.Ok(g) => g.n, Result.Err(e) => e } }
fn c6() -> i64 { let mut r = mkr(6); r = mkr(66); match r { Result.Ok(g) => g.n, Result.Err(e) => e } }
fn c7() -> i64 { let r = mkr(7); match r { Result.Ok(g) => take(g), Result.Err(e) => e } }
fn c8() -> i64 { let r = mkr(8); let s = match r { Result.Ok(G { v, n }) => v, Result.Err(e) => mk(e) }; s.len() }
fn c9() -> i64 { eat(mkr(9)) }
fn c10() -> i64 { let r = mkr(10); eat(r) }
fn c11() -> i64 { match mkr(11) { Result.Ok(g) => g.v.len(), Result.Err(e) => e } }
fn c12() -> i64 { match mkr(12) { Result.Ok(g) => { println(g.v); g.n }, Result.Err(e) => e } }
fn c13() -> i64 { match mke(13) { Result.Ok(n) => n, Result.Err(e) => { println(e.v); 13 } } }
fn c14() -> i64 { match mkr(14) { Result.Ok(_) => 14, Result.Err(e) => e } }
fn main() {
    println(f"{c1()} {c2()} {c3()} {c4()} {c5()} {c6()} {c7()} {c8()} {c9()} {c10()}");
    println(f"{c11()} {c12()} {c13()} {c14()}");
    println("end");
}"#,
        &[
            "1 2 3 71 5 66 31 31 9 10",
            "a-heap-string-longer-than-sso-12",
            "a-heap-string-longer-than-sso-13",
            "32 12 13 14",
            "end",
        ],
        "asan_generic_struct_inline_result_payload_is_dropped_at_instantiation",
        3,
    );
}
