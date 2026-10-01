//! B-2026-10-01-25 -- a by-value param, or its payload, moved into a DISCARDED
//! literal or constructor runs its `Drop` body once, in the caller.

use super::*;

/// B-2026-10-01-25 — design.md § Drop ordering rule 3: a by-value param's body
/// runs in the caller when the call returns, including when the callee moves it
/// into a local that dies inside the frame. A DISCARDED literal or constructor
/// (`let _ = [w];`, `(w, 1);`, `let _ = E.A(w);`) over the param ran it a second
/// time inside the callee on every surface for a tuple, struct literal and
/// constructor, under `--interp` alone for an array or `Vec`, and for the payload
/// binding of a by-value `Option` or `Result` param it ran inside the callee
/// (once compiled, twice under `--interp`); `let _ = w;` over such a payload ran
/// it nowhere compiled. Every cell prints its function's line, then the body,
/// then the caller's `x`.
#[test]
fn e2e_discarded_literal_of_param_view_runs_body_once_in_caller() {
    let src = r#"struct W1 { v: i64, s: String }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
fn mk(n: i64) -> W1 { return W1 { v: n, s: f"ssssssssssssssssssssssssssssss{n}" } }
enum E { A(W1), B }
enum D { A(W1), B }
impl Drop for D { fn drop(mut ref self) { println("dD") } }
struct S2 { r: W1, k: i64 }
fn d1(w: W1) { let _ = [w]; println("d1") }
fn d2(w: W1) { [w]; println("d2") }
fn d3(w: W1) { let _ = (w, 1); println("d3") }
fn d4(w: W1) { (w, mk(40)); println("d4") }
fn d5(w: W1) { let _ = S2 { r: w, k: 1 }; println("d5") }
fn d6(w: W1) { let _ = E.A(w); println("d6") }
fn d7(w: W1) { D.A(w); println("d7") }
fn d8(w: W1) { let _ = Vec[w]; println("d8") }
fn p1(o: Option[W1]) { match o { Some(w) => { let _ = w; println("p1") }, None => {} } }
fn p2(o: Option[W1]) { match o { Some(w) => { let _ = [w]; println("p2") }, None => {} } }
fn p3(o: Option[W1]) { match o { Some(w) => { [w]; println("p3") }, None => {} } }
fn p4(o: Option[W1]) { match o { Some(w) => { let _ = E.A(w); println("p4") }, None => {} } }
fn p5(o: Option[W1]) { match o { Some(w) => { let _ = Some(w); println("p5") }, None => {} } }
fn p6(o: Option[W1]) { match o { Some(w) => { let _ = (w, 1); println("p6") }, None => {} } }
fn p7(o: Option[W1], c: bool) { match o { Some(w) => { if c { let _ = [w]; println("p7t") }; println("p7") }, None => {} } }
fn p8(o: Option[W1]) { if let Some(w) = o { let _ = S2 { r: w, k: 2 }; println("p8") } }
fn p9(r: Result[W1, i64]) { match r { Ok(w) => { let _ = [w]; println("p9") }, Err(_) => {} } }
fn e1(e: E) { match e { E.A(w) => { let _ = (w, 1); println("e1") }, E.B => {} } }
fn main() {
    d1(mk(1)); println("x"); d2(mk(2)); println("x"); d3(mk(3)); println("x"); d4(mk(4)); println("x")
    d5(mk(5)); println("x"); d6(mk(6)); println("x"); d7(mk(7)); println("x"); d8(mk(8)); println("x")
    p1(Some(mk(11))); println("x"); p2(Some(mk(12))); println("x"); p3(Some(mk(13))); println("x")
    p4(Some(mk(14))); println("x"); p5(Some(mk(15))); println("x"); p6(Some(mk(16))); println("x")
    p7(Some(mk(17)), true); println("x"); p7(Some(mk(18)), false); println("x")
    p8(Some(mk(19))); println("x"); p9(Ok(mk(20))); println("x"); e1(E.A(mk(21))); println("x")
    let a = mk(22); d3(a); println("x")
    let b = Some(mk(23)); p2(b); println("x")
    println("end")
}
"#;
    let want = "d1\ndW1_1\nx\nd2\ndW1_2\nx\nd3\ndW1_3\nx\ndW1_40\nd4\ndW1_4\nx\nd5\ndW1_5\nx\nd6\ndW1_6\nx\ndD\nd7\ndW1_7\nx\nd8\ndW1_8\nx\np1\ndW1_11\nx\np2\ndW1_12\nx\np3\ndW1_13\nx\np4\ndW1_14\nx\np5\ndW1_15\nx\np6\ndW1_16\nx\np7t\np7\ndW1_17\nx\np7\ndW1_18\nx\np8\ndW1_19\nx\np9\ndW1_20\nx\ne1\ndW1_21\nx\nd3\ndW1_22\nx\np2\ndW1_23\nx\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
