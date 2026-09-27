//! B-2026-09-27-94 — a fresh-temp boxed `Option` argument to a param the
//! callee returns on only some paths is freed once on either path.

use super::*;

/// B-2026-09-27-94 — `mf(Some(mk(2)), false)` over `fn mf(a: Option[S], c:
/// bool) -> Option[S] { if c { a } else { None } }`: the callee runs the
/// payload's bodies on the leg that does not return it and frees nothing, and
/// the caller stood its temp's owner down because the param may flow into the
/// return, so the box and its `String` were nobody's (61 B per call). The caller
/// now owns the temp on both legs and the post-call compare of the returned box
/// word disarms it on the leg that hands it back. Covers the free, method,
/// associated, early-return, nested, aggregate-return and loop spellings, with a
/// named-argument control.
#[test]
fn e2e_freshtemp_boxed_option_conditionally_returned_runs_bodies_once() {
    let src = r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
struct H { n: i64 }
impl H {
    fn mf(ref self, a: Option[S], c: bool) -> Option[S] { if c { a } else { None } }
    fn sf(a: Option[S], c: bool) -> Option[S] { if c { a } else { None } }
}
struct W { o: Option[S], n: i64 }
fn mf(a: Option[S], c: bool) -> Option[S] { if c { a } else { None } }
fn me(a: Option[S], c: bool) -> Option[S] { if c { return a } println("in"); None }
fn mw(a: Option[S], c: bool) -> W { if c { W { o: a, n: 1 } } else { W { o: None, n: 2 } } }
fn show(o: Option[S]) { match o { Some(v) => println(f"got{v.r.id}"), None => println("none") } }
fn main() {
    let h = H { n: 0 };
    show(mf(Some(mk(1)), true));
    show(mf(Some(mk(2)), false));
    let b: Option[S] = Some(mk(3));
    show(mf(b, false));
    show(h.mf(Some(mk(4)), true));
    show(h.mf(Some(mk(5)), false));
    show(H.sf(Some(mk(6)), false));
    show(me(Some(mk(7)), false));
    show(mf(mf(Some(mk(8)), true), false));
    let w = mw(Some(mk(9)), false);
    println(f"w{w.n}");
    let mut i = 10;
    while i < 12 { show(mf(Some(mk(i)), i == 11)); i += 1; }
    println("end")
}"#;
    let want = "got1\nd1\nd2\nnone\nd3\nnone\ngot4\nd4\nd5\nnone\nd6\nnone\nin\nd7\nnone\nd8\nnone\nd9\nw2\nd10\nnone\ngot11\nd11\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    if let Some(aot) = run_program(src) {
        assert_eq!(aot, want, "AOT");
    }
}
