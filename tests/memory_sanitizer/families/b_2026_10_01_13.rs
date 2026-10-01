//! B-2026-10-01-13 -- a tuple bound from a `match` whose element is read
//! through a field: freed once.

use super::*;

/// B-2026-10-01-13 — the memory half. The programs did not build before the
/// fix; this pins that the element each arm hands in is freed exactly once now
/// that they do, including the tuple whose every arm fills the element with
/// its own payload binding. Same program as the codegen twin.
#[test]
fn asan_field_read_through_tuple_bound_from_match_freed_once() {
    assert_clean_asan_run(
        r#"struct W1 { v: i64, s: String }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
fn mk(n: i64) -> W1 { return W1 { v: n, s: f"ssssssssssssssssssssssssssssss{n}" } }
fn some_first(n: i64) {
    let o = Some(mk(n));
    let t = match o { Some(w) => (w, 1), None => (mk(0), 0) };
    println(f"sf{t.0.v} {t.1}")
}
fn none_first(n: i64) {
    let o = Some(mk(n));
    let t = match o { None => (mk(0), 0), Some(w) => (w, 1) };
    println(f"nf{t.0.v} {t.1}")
}
fn param(o: Option[W1]) {
    let t = match o { Some(w) => (w, 1), None => (mk(0), 0) };
    println(f"pa{t.0.v} {t.1}")
}
fn if_let(n: i64) {
    let o = Some(mk(n));
    let t = if let Some(w) = o { (w, 1) } else { (mk(0), 0) };
    println(f"il{t.0.v} {t.0.s.len()}")
}
fn braced(n: i64) {
    let o = Some(mk(n));
    let t = { let z = 3; match o { Some(w) => (w, z), None => (mk(0), 0) } };
    println(f"bl{t.0.v} {t.1}")
}
fn payload_only(n: i64) {
    let r: Result[W1, W1] = Err(mk(n));
    let t = match r { Ok(w) => (w, 1), Err(w) => (w, 2) };
    println(f"po{t.0.v} {t.1}")
}
fn second(c: i64) {
    let t = match c { 1 => (1, mk(1)), _ => (2, mk(c)) };
    println(f"se{t.0} {t.1.v}")
}
fn main() {
    some_first(1);
    none_first(2);
    param(Some(mk(3)));
    param(None);
    if_let(4);
    braced(5);
    payload_only(6);
    second(7);
    println("end")
}
"#,
        &[
            "sf1 1", "dW1_1", "nf2 1", "dW1_2", "pa3 1", "dW1_3", "pa0 0", "dW1_0", "il4 31",
            "dW1_4", "bl5 3", "dW1_5", "po6 2", "dW1_6", "se2 7", "dW1_7", "end",
        ],
        "field_read_through_tuple_bound_from_match",
    );
}
