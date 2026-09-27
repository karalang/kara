//! B-2026-09-27-128 — a generic fn that returns its `Option[T]` / `Result[T,
//! E]` param on only some paths runs the payload's `Drop` body on the path
//! that does not return it, and frees a fresh temp's box.

use super::*;

/// B-2026-09-27-128 — the ASAN twin of
/// `e2e_generic_conditionally_returned_optres_param_runs_body_on_other_path`:
/// a fresh temp's box and `String` are freed once on either leg, which the
/// generic call site now owns and disarms with the returned-box-word compare.
#[test]
fn asan_generic_conditionally_returned_optres_param_is_freed() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64 }
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
}
"#,
        &[
            "got1", "d1", "d2", "none", "d3", "none", "got4", "d4", "d5", "none", "in", "d6",
            "none", "d7", "none", "d8", "rtrue", "d9", "e7", "d10", "none", "got11", "d11", "end",
        ],
        "asan_generic_conditionally_returned_optres_param_is_freed",
        14,
    );
}
