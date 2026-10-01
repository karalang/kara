//! B-2026-10-01-25 -- a by-value param, or its payload, moved into a DISCARDED
//! literal or constructor runs its `Drop` body once, in the caller.

use super::*;

/// B-2026-10-01-25 — the memory half: the discarded literal now owns this frame's
/// copy of a param view it holds, so `let _ = [w];` no longer leaks the literal's
/// buffer and the element's `String`, and every other spelling frees each copy
/// once. Same program as the codegen twin.
#[test]
fn asan_discarded_literal_of_param_view_freed_once() {
    assert_clean_asan_run(
        r#"struct W1 { v: i64, s: String }
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
"#,
        &[
            "d1", "dW1_1", "x", "d2", "dW1_2", "x", "d3", "dW1_3", "x", "dW1_40", "d4", "dW1_4",
            "x", "d5", "dW1_5", "x", "d6", "dW1_6", "x", "dD", "d7", "dW1_7", "x", "d8", "dW1_8",
            "x", "p1", "dW1_11", "x", "p2", "dW1_12", "x", "p3", "dW1_13", "x", "p4", "dW1_14",
            "x", "p5", "dW1_15", "x", "p6", "dW1_16", "x", "p7t", "p7", "dW1_17", "x", "p7",
            "dW1_18", "x", "p8", "dW1_19", "x", "p9", "dW1_20", "x", "e1", "dW1_21", "x", "d3",
            "dW1_22", "x", "p2", "dW1_23", "x", "end",
        ],
        "discarded_literal_of_param_view",
    );
}
