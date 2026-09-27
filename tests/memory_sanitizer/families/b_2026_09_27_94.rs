//! B-2026-09-27-94 — a fresh-temp boxed `Option` argument to a param the
//! callee returns on only some paths is freed once on either path.

use super::*;

/// B-2026-09-27-94 — the ASAN twin of
/// `e2e_freshtemp_boxed_option_conditionally_returned_runs_bodies_once`: the
/// temp's box and `String` are freed once whichever leg the callee takes.
#[test]
fn asan_freshtemp_boxed_option_conditionally_returned_is_freed() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64 }
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
}
"#,
        &[
            "got1", "d1", "d2", "none", "d3", "none", "got4", "d4", "d5", "none", "d6", "none",
            "in", "d7", "none", "d8", "none", "d9", "w2", "d10", "none", "got11", "d11", "end",
        ],
        "asan_freshtemp_boxed_option_conditionally_returned_is_freed",
        20,
    );
}
