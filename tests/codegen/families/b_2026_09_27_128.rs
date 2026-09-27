//! B-2026-09-27-128 — a generic fn that returns its `Option[T]` / `Result[T,
//! E]` param on only some paths runs the payload's `Drop` body on the path
//! that does not return it, and frees a fresh temp's box.

use super::*;

/// B-2026-09-27-128 — `mg(Some(mk(2)), false)` over `fn mg[T](a: Option[T], c:
/// bool) -> Option[T] { if c { a } else { None } }` printed no `d2` on any
/// surface: the caller stands the payload's bodies down (the param may be
/// handed back), and the callee registered nothing for the leg that drops it.
/// The non-generic twin was right because its declared type names the payload;
/// here it is `T`. Codegen's monomorph prologue now registers the bodies off
/// the SUBSTITUTED type, and the interpreter seeds its payload walk off the
/// bound value's type. Covers a free fn, a generic method, an early `return`,
/// a named argument, a nested call, an inline `Option[R]`, a `Result[T, i64]`
/// and a loop.
#[test]
fn e2e_generic_conditionally_returned_optres_param_runs_body_on_other_path() {
    let src = r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
struct H { n: i64 }
impl H {
    fn mg[T](ref self, a: Option[T], c: bool) -> Option[T] { if c { a } else { None } }
}
fn mg[T](a: Option[T], c: bool) -> Option[T] { if c { a } else { None } }
fn me[T](a: Option[T], c: bool) -> Option[T] { if c { return a } println("in"); None }
fn mr[T](a: Result[T, i64], c: bool) -> Result[T, i64] { if c { a } else { Err(7) } }
fn show(o: Option[S]) { match o { Some(v) => println(f"got{v.r.id}"), None => println("none") } }
fn main() {
    let h = H { n: 0 };
    show(mg(Some(mk(1)), true));
    show(mg(Some(mk(2)), false));
    let b: Option[S] = Some(mk(3));
    show(mg(b, false));
    show(h.mg(Some(mk(4)), true));
    show(h.mg(Some(mk(5)), false));
    show(me(Some(mk(6)), false));
    show(mg(mg(Some(mk(7)), true), false));
    let r = mg(Some(R { id: 8 }), false);
    println(f"r{r.is_none()}");
    let e: Result[S, i64] = Ok(mk(9));
    match mr(e, false) { Ok(v) => println(f"ok{v.r.id}"), Err(x) => println(f"e{x}") }
    let mut i = 10;
    while i < 12 { show(mg(Some(mk(i)), i == 11)); i += 1; }
    println("end")
}"#;
    let want = "got1\nd1\nd2\nnone\nd3\nnone\ngot4\nd4\nd5\nnone\nin\nd6\nnone\nd7\nnone\nd8\nrtrue\nd9\ne7\nd10\nnone\ngot11\nd11\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    if let Some(aot) = run_program(src) {
        assert_eq!(aot, want, "AOT");
    }
}
