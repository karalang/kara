//! B-2026-09-28-13 / B-2026-09-28-34 — an identity hand-back of a by-value
//! `Option`/`Result` param (`id(a)` over `fn id(a: Option[S]) -> Option[S]
//! { a }`) is that param's own envelope, so it behaves exactly like `a` in
//! every position; and a param the callee only drops or probes
//! (`o.is_some()`, `o;`, `let _ = o;`) leaves the caller owing the payload's
//! `Drop` body.

use super::*;

/// B-2026-09-28-13 — `id(a)` discarded, probed, bound, passed on, missed,
/// matched, `if let` / `while let` bound, and the `Result` twin, each called
/// with a fresh temp and (where the shape allows) a named argument. Before the
/// fix these doubled the body, lost it, or double-freed, differently per
/// surface (4 invalid frees for `id(a);` at `-O0`); after it each body runs once, at the caller.
#[test]
fn asan_optres_identity_handback_of_param_runs_body_once_at_caller() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mks(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn id(a: Option[S]) -> Option[S] { a }
fn idr(a: Option[R]) -> Option[R] { a }
fn idres(a: Result[S, i64]) -> Result[S, i64] { a }
fn eat(o: Option[S]) { println("eat") }
fn keep(x: S) { println("kp") }
fn g(o: Option[S]) -> i64 { if let Some(x) = o { x.r.id } else { 0 } }
fn c1(a: Option[S]) { id(a); println("in1") }
fn c2(a: Option[S]) -> bool { id(a).is_some() }
fn c3(a: Option[S]) { let _ = id(a); println("in3") }
fn c4(a: Option[S]) { eat(id(a)); println("in4") }
fn c5(a: Option[S]) -> bool { if let None = id(a) { false } else { true } }
fn c6(a: Option[R]) { idr(a); println("in6") }
fn c7(a: Option[S]) { let b = id(a); println("in7") }
fn c8(a: Option[S]) { match id(a) { Some(x) => println(f"k{x.r.id}"), None => {} }; println("in8") }
fn c9(a: Option[S]) { match id(a) { Some(x) => keep(x), None => {} }; println("in9") }
fn c10(a: Option[S]) -> Option[S] { match id(a) { Some(x) => Some(x), None => None } }
fn c11(a: Option[S]) -> i64 { if let Some(x) = id(a) { x.r.id } else { 0 } }
fn c12(a: Option[S]) -> i64 { let b = id(a); if let Some(x) = b { x.r.id } else { 0 } }
fn c13(a: Option[S]) -> i64 { g(id(a)) }
fn c14(a: Option[S]) -> i64 { let mut n = 0; while let Some(x) = id(a) { n = x.r.id; break } n }
fn c15(a: Result[S, i64]) -> i64 { match idres(a) { Ok(x) => x.r.id, Err(e) => e } }
fn c16(a: Result[S, i64]) -> i64 { if let Ok(x) = idres(a) { x.r.id } else { 0 } }
fn c17(a: Result[S, i64]) -> bool { idres(a).is_ok() }
fn main() {
    c1(Some(mks(1))); println("o1"); let a1 = Some(mks(2)); c1(a1);
    println(f"a{c2(Some(mks(3)))}"); let a2 = Some(mks(4)); println(f"a{c2(a2)}");
    c3(Some(mks(5))); println("o3");
    c4(Some(mks(6))); println("o4");
    println(f"a{c5(Some(mks(7)))}");
    c6(Some(R { id: 8 })); println("o6");
    c7(Some(mks(9))); println("o7"); let a7 = Some(mks(10)); c7(a7);
    c8(Some(mks(11))); println("o8");
    c9(Some(mks(12))); println("o9");
    let r10 = c10(Some(mks(13))); println("o10");
    println(f"k{c11(Some(mks(14)))}"); let a11 = Some(mks(15)); println(f"k{c11(a11)}");
    println(f"k{c12(Some(mks(16)))}");
    println(f"k{c13(Some(mks(17)))}");
    println(f"k{c14(Some(mks(18)))}");
    println(f"k{c15(Ok(mks(19)))}"); let a15: Result[S, i64] = Ok(mks(20)); println(f"k{c15(a15)}");
    println(f"k{c16(Ok(mks(21)))}");
    println(f"a{c17(Ok(mks(22)))}");
    println("end")
}
"#,
        &[
            "in1", "d1", "o1", "in1", "d2", "d3", "atrue", "atrue", "d4", "in3", "d5", "o3", "eat",
            "in4", "d6", "o4", "d7", "atrue", "in6", "d8", "o6", "in7", "d9", "o7", "in7", "d10",
            "k11", "in8", "d11", "o8", "kp", "in9", "d12", "o9", "d13", "o10", "d14", "k14", "k15",
            "d15", "d16", "k16", "d17", "k17", "d18", "k18", "d19", "k19", "k20", "d20", "d21",
            "k21", "d22", "atrue", "end",
        ],
        "asan_optres_identity_handback_of_param_runs_body_once_at_caller",
        20,
    );
}

/// B-2026-09-28-34 — a callee that only probes (`o.is_some()`, `o.is_ok()`)
/// or discards (`o;`, `let _ = o;`) its by-value `Option`/`Result` param ran no
/// payload `Drop` body for a fresh temp on any compiled surface.
#[test]
fn asan_optres_param_only_probed_or_discarded_runs_body_at_caller() {
    assert_clean_asan_run_min_allocs(
        r#"
struct W { id: i64, name: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}/{self.name}") } }
fn mk(i: i64) -> W { return W { id: i, name: f"n{i}" }; }
fn so(o: Option[W]) -> bool { return o.is_some(); }
fn ro(o: Result[W, i64]) -> bool { return o.is_ok(); }
fn rt(o: Result[(W, i64), i64]) -> bool { return o.is_ok(); }
fn dd(o: Option[W]) -> i64 { o; return 1; }
fn lw(o: Result[W, i64]) -> i64 { let _ = o; return 2; }
fn main() { println(f"a{so(Some(mk(1)))}"); println(f"b{ro(Ok(mk(2)))}"); println(f"c{rt(Ok((mk(3), 9)))}"); println(f"d{dd(Some(mk(4)))}"); println(f"e{lw(Ok(mk(5)))}"); let n = Some(mk(6)); println(f"f{so(n)}"); println("end") }
"#,
        &[
            "dW1/n1", "atrue", "dW2/n2", "btrue", "dW3/n3", "ctrue", "dW4/n4", "d1", "dW5/n5",
            "e2", "ftrue", "dW6/n6", "end",
        ],
        "asan_optres_param_only_probed_or_discarded_runs_body_at_caller",
        6,
    );
}
