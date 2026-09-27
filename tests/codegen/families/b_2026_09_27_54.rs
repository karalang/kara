//! B-2026-09-27-54 — a fresh-temp boxed `Option` / `Result` argument to a
//! by-value METHOD or ASSOCIATED-function param.

use super::*;

/// B-2026-09-27-54 — a fresh-temp boxed `Option` / `Result` argument to a
/// by-value METHOD or ASSOCIATED-function param. Only the free-function
/// argument loop registered the caller's owner of the temp's box, so every
/// compiled surface leaked it (32 B per call); `--interp` ran the payload body
/// twice when the caller's dead binding shared the param's name.
#[test]
fn e2e_boxed_optres_temp_to_method_param_runs_body_once() {
    let src = r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
struct W { r: R, s: String, t: String }
fn mw(i: i64) -> W { W { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}", t: f"another-long-heap-string-{i}" } }
struct H { n: i64 }
impl H {
    fn look(ref self, a: Option[S]) -> i64 { println("in"); self.n }
    fn eat(self, a: Option[S]) -> i64 { println("in"); self.n }
    fn take(ref self, a: Option[S]) -> i64 { match a { Some(s) => s.r.id, None => 0 } }
    fn res(ref self, a: Result[W, i64]) -> i64 { match a { Ok(w) => w.r.id, Err(e) => e } }
    fn assoc(a: Option[S]) -> i64 { match a { Some(s) => s.r.id, None => 0 } }
}
fn main() {
    let h = H { n: 5 };
    let a: Option[S] = Some(mk(1));
    println(f"k{h.look(a)}");
    println(f"k{h.look(Some(mk(2)))}");
    println(f"k{H { n: 6 }.eat(Some(mk(3)))}");
    println(f"k{h.take(Some(mk(4)))}");
    println(f"k{h.take(None)}");
    println(f"k{h.res(Ok(mw(5)))}");
    println(f"k{h.res(Err(7))}");
    println(f"k{H.assoc(Some(mk(6)))}");
    println(f"k{H.assoc(None)}");
    println("end")
}"#;
    let want = "in\nk5\nd1\nin\nd2\nk5\nin\nd3\nk6\nd4\nk4\nk0\nd5\nk5\nk7\nd6\nk6\nk0\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    if let Some(aot) = run_program(src) {
        assert_eq!(aot, want, "AOT");
    }
}
