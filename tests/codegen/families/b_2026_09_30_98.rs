//! B-2026-09-30-98 -- an `Option`/`Result` payload binding moved into a tuple
//! hands the box's interior to the tuple, so it is freed once.

use super::*;

/// B-2026-09-30-98 — `match o { Some(w) => { let _ = (w, 1); .. } .. }`
/// aborted `free(): double free detected in tcache 2` under `karac run`: the
/// tuple's drop freed `w`'s `String` and the boxed payload's inner walk freed
/// it again. The struct-literal spelling was clean. Covers `let _ =`, the bare
/// statement, a bound tuple, `if let`, a `Result` payload, a heap-only payload,
/// a tuple pushed into a `Vec`, and an arm nested under an `if`.
#[test]
fn e2e_payload_binding_moved_into_tuple_runs_one_body() {
    let src = r#"struct W1 { v: i64, s: String }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct H1 { v: i64, s: String }
fn mk(n: i64) -> W1 { return W1 { v: n, s: f"ssssssssssssssssssssssssssssss{n}" } }
fn mh(n: i64) -> H1 { return H1 { v: n, s: f"hhhhhhhhhhhhhhhhhhhhhhhhhhhhhh{n}" } }
fn nested(c: bool) {
    let o = Some(mk(7));
    if c { match o { Some(w) => { let _ = (w, 1); println("n") }, None => {} } };
    println("out")
}
fn main() {
    let o1 = Some(mk(1));
    match o1 { Some(w) => { let _ = (w, 1); println("a1") }, None => {} };
    let o2 = Some(mk(2));
    match o2 { Some(w) => { (w, 1); println("a2") }, None => {} };
    let o3 = Some(mk(3));
    match o3 { Some(w) => { let t = (w, 1); println(f"a3 {t.1}") }, None => {} };
    let o4 = Some(mk(4));
    if let Some(w) = o4 { let t = (w, 4); println(f"a4 {t.1}") };
    let r: Result[W1, i64] = Ok(mk(5));
    match r { Ok(w) => { let _ = (w, 5); println("a5") }, Err(e) => { println(f"{e}") } };
    let o6 = Some(mh(6));
    match o6 { Some(h) => { let t = (h, 6); println(f"a6 {t.0.v}") }, None => {} };
    let mut v: Vec[(W1, i64)] = Vec.new();
    let o8 = Some(mk(8));
    match o8 { Some(w) => { v.push((w, 8)) }, None => {} };
    println(f"a8 {v.len()}");
    nested(true);
    nested(false);
    println("end")
}
"#;
    let want = "dW1_1\na1\ndW1_2\na2\na3 1\ndW1_3\na4 4\ndW1_4\ndW1_5\na5\na6 6\na8 1\ndW1_8\ndW1_7\nn\nout\ndW1_7\nout\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
