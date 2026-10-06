//! B-2026-10-06-129: a generic param wrapped in an `Option` / `Result` local and handed back on some exits runs one body

use super::*;

/// B-2026-10-06-129 — a generic function that wraps its bare-`T` param in an
/// `Option` / `Result` local and hands that local back on some exits only
/// (`fn cg[T](s: T, c: bool) -> Option[T] { let o = Some(s); if c { return o }
/// return None }`). Since B-2026-10-05-107 let the param fate answer for a
/// bare `T`, the caller stands down on every path, but neither backend's
/// per-path carrier recognised the wrapper's declared `Option[T]`, so the exit
/// that returns `None` ran no body: `cg(P { id: 2 }, false)` printed `sfalse
/// end` on every surface. Each body now runs once on both paths, for an
/// `Ok` / `Err` wrap, a nest (`Some(Some(s))`), a rebind of the wrapper, an
/// enum payload handed back, and a named argument.
#[test]
fn asan_generic_param_wrapped_in_local_handed_back_on_some_exits_runs_one_body() {
    assert_clean_asan_run(
        r#"struct P { id: i64 }
impl Drop for P { fn drop(mut ref self) { println(f"dP{self.id}") } }
enum E { A(P), B }
fn cg[T](s: T, c: bool) -> Option[T] { let o = Some(s); if c { return o } return None }
fn cr[T](s: T, c: bool) -> Result[T, i64] { let o: Result[T, i64] = Ok(s); if c { return o } return Err(0) }
fn ce[T](s: T, c: bool) -> Result[i64, T] { let o: Result[i64, T] = Err(s); if c { return o } return Ok(0) }
fn cn[T](s: T, c: bool) -> Option[Option[T]] { let o = Some(Some(s)); if c { return o } return None }
fn cq[T](s: T, c: bool) -> Option[T] { let o = Some(s); let q = o; if c { return q } return None }
fn ca[T](s: T) -> Option[T] { let o = Some(s); return o }
fn c1() { let p = cg(P { id: 1 }, false); println(f"_1 {p.is_some()}"); }
fn c2() { let p = cg(P { id: 2 }, true); println(f"_2 {p.is_some()}"); }
fn c3() { let x = P { id: 3 }; let p = cg(x, false); println(f"_3 {p.is_some()}"); }
fn c4() { let x = P { id: 4 }; let p = cg(x, true); println(f"_4 {p.is_some()}"); }
fn c5() { let p = cr(P { id: 5 }, false); println(f"_5 {p.is_ok()}"); }
fn c6() { let p = cr(P { id: 6 }, true); println(f"_6 {p.is_ok()}"); }
fn c7() { let p = ce(P { id: 7 }, false); println(f"_7 {p.is_ok()}"); }
fn c8() { let p = ce(P { id: 8 }, true); println(f"_8 {p.is_ok()}"); }
fn c9() { let p = cn(P { id: 9 }, false); println(f"_9 {p.is_some()}"); }
fn c10() { let p = cn(P { id: 10 }, true); println(f"_10 {p.is_some()}"); }
fn c11() { let p = cq(P { id: 11 }, false); println(f"_11 {p.is_some()}"); }
fn c12() { let p = cq(P { id: 12 }, true); println(f"_12 {p.is_some()}"); }
fn c13() { let p = cg(E.A(P { id: 13 }), true); println(f"_13 {p.is_some()}"); }
fn c14() { let p = ca(P { id: 14 }); println(f"_14 {p.is_some()}"); }
fn c15() { let x = P { id: 15 }; let p = cg(x, false); let r = cg(P { id: 16 }, true); println(f"_15 {p.is_some()} {r.is_some()}"); }
fn c16() { let p = cg("s", false); let q = cg(17, true); println(f"_17 {p.is_some()} {q.is_some()}"); }
fn main() { c1(); c2(); c3(); c4(); c5(); c6(); c7(); c8(); c9(); c10(); c11(); c12(); c13(); c14(); c15(); c16(); println("end"); }
"#,
        &[
            "dP1",
            "_1 false",
            "_2 true",
            "dP2",
            "dP3",
            "_3 false",
            "_4 true",
            "dP4",
            "dP5",
            "_5 false",
            "_6 true",
            "dP6",
            "dP7",
            "_7 true",
            "_8 false",
            "dP8",
            "dP9",
            "_9 false",
            "_10 true",
            "dP10",
            "dP11",
            "_11 false",
            "_12 true",
            "dP12",
            "_13 true",
            "dP13",
            "_14 true",
            "dP14",
            "dP15",
            "_15 false true",
            "dP16",
            "_17 false true",
            "end",
        ],
        "asan_generic_param_wrapped_in_local_handed_back_on_some_exits_runs_one_body",
    );
}
