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
fn interp_generic_param_wrapped_in_local_handed_back_on_some_exits_runs_one_body() {
    let out = run(r#"struct P { id: i64 }
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
"#);
    assert_eq!(out, "dP1\n_1 false\n_2 true\ndP2\ndP3\n_3 false\n_4 true\ndP4\ndP5\n_5 false\n_6 true\ndP6\ndP7\n_7 true\n_8 false\ndP8\ndP9\n_9 false\n_10 true\ndP10\ndP11\n_11 false\n_12 true\ndP12\n_13 true\ndP13\n_14 true\ndP14\ndP15\n_15 false true\ndP16\n_17 false true\nend\n", "got:\n{out}");
}
