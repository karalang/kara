//! B-2026-09-30-66 -- an `Option`/`Result` payload binding moved into a
//! collection literal: freed once, and its body runs once.

use super::*;

/// B-2026-09-30-66 — `Some(x) => { let z = pass([x]); .. }` over a callee that
/// hands the array back ran `x`'s body twice compiled (the result's walk and
/// the payload view's) and freed its heap twice on the JIT (the result's walk
/// and the box's). A value that dies in the callee (`take([x])`) keeps both
/// with the scrutinee. Covers `Array` / `Vec` / `Vec[..]` literals, a generic
/// hand-back, a `Result` payload, `if let`, a discarded hand-back, and the
/// let-bound literal `let z = [x]`, which double-freed on the JIT.
#[test]
fn e2e_payload_binding_moved_into_collection_literal_runs_one_body() {
    let src = r#"struct W1 { v: i64, s: String }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
fn pass(x: Array[W1, 1]) -> Array[W1, 1] { return x }
fn take(x: Array[W1, 1]) -> i64 { return x[0].v }
fn passv(x: Vec[W1]) -> Vec[W1] { return x }
fn pg[T](x: T) -> T { return x }
fn mk(n: i64) -> W1 { return W1 { v: n, s: f"ssssssssssssssssssssssssssssss{n}" } }
fn main() {
    let o1 = Some(mk(1));
    match o1 { Some(x) => { let z = pass([x]); println(f"z:{z[0].v}") }, None => {} };
    let o2 = Some(mk(2));
    match o2 { Some(x) => { let r = take([x]); println(f"r:{r}") }, None => {} };
    let o3 = Some(mk(3));
    match o3 { Some(x) => { let z = passv([x]); println(f"z:{z[0].v}") }, None => {} };
    let o4 = Some(mk(4));
    match o4 { Some(x) => { let z = passv(Vec[x]); println(f"z:{z[0].v}") }, None => {} };
    let o5 = Some(mk(5));
    match o5 { Some(x) => { let z = pg([x]); println(f"z:{z[0].v}") }, None => {} };
    let r6: Result[W1, i64] = Ok(mk(6));
    match r6 { Ok(x) => { let z = pass([x]); println(f"z:{z[0].v}") }, Err(e) => { println(f"{e}") } };
    let o7 = Some(mk(7));
    if let Some(x) = o7 { let z = pass([x]); println(f"z:{z[0].v}") };
    let o8 = Some(mk(8));
    match o8 { Some(x) => { pass([x]); println("d8") }, None => {} };
    let o9 = Some(mk(9));
    match o9 { Some(x) => { let z = [x]; println(f"z:{z[0].v}") }, None => {} };
    let o10 = Some(mk(10));
    match o10 { Some(x) => { let z: Array[W1, 1] = Array[x]; let y = z; println(f"y:{y[0].v}") }, None => {} };
    let o11 = Some(mk(11));
    match o11 { Some(x) => { let z = Vec[x]; println(f"n:{z.len()}") }, None => {} };
    println("end")
}
"#;
    let want = "z:1\ndW1_1\nr:2\ndW1_2\nz:3\ndW1_3\nz:4\ndW1_4\nz:5\ndW1_5\nz:6\ndW1_6\nz:7\ndW1_7\ndW1_8\nd8\nz:9\ndW1_9\ny:10\ndW1_10\nn:1\ndW1_11\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
