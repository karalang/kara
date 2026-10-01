//! B-2026-09-30-106 -- an `Option`/`Result`/enum payload binding moved into
//! an aggregate literal at a `match` arm's or `if let`'s tail runs its body
//! once, where the result dies.

use super::*;

/// B-2026-09-30-106 — `Some(w) => (w, 1)` ran `w`'s body at the arm's end
/// (before the result was read) and the result's own walk ran it again, on
/// every compiled backend. Also covered: a struct literal and an array
/// literal (`[w]`, which double freed on the JIT, braced too), an inline
/// `Drop` payload, a user enum and a `Result`, `if let`, the `None` path,
/// a loop, and the UNANNOTATED binding, whose tuple type was named from the
/// first arm only and so had no walk once the arm stopped running the body.
#[test]
fn e2e_payload_binding_moved_into_arm_tail_literal_runs_body_once() {
    let src = r#"struct W1 { v: i64, s: String }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
fn mk(n: i64) -> W1 { return W1 { v: n, s: f"ssssssssssssssssssssssssssssss{n}" } }
struct W2 { v: i64 }
impl Drop for W2 { fn drop(mut ref self) { println(f"dW2_{self.v}") } }
struct P { w: W1, k: i64 }
enum E { A(W1), B }
fn bare_tuple(o: Option[W1]) {
    let t = match o { Some(w) => (w, 1), None => (mk(0), 0) };
    println(f"bt{t.1}")
}
fn read_tuple(o: Option[W1]) {
    let t: (W1, i64) = match o { Some(w) => (w, 1), None => (mk(0), 0) };
    println(f"rt{t.0.v} {t.1}")
}
fn struct_tail(o: Option[W1]) {
    let t = match o { Some(w) => P { w: w, k: 1 }, None => P { w: mk(0), k: 0 } };
    println(f"st{t.w.v} {t.k}")
}
fn array_tail(n: i64) {
    let o = Some(mk(n));
    let t = match o { Some(w) => [w], None => [mk(0)] };
    println(f"at{t[0].v}")
}
fn braced_array(n: i64) {
    let o = Some(mk(n));
    let t = match o { Some(w) => { [w] }, None => [mk(0)] };
    println(f"ba{t[0].v}")
}
fn inline_payload(o: Option[W2]) {
    let t: (W2, i64) = match o { Some(w) => (w, 1), None => (W2 { v: 0 }, 0) };
    println(f"ip{t.0.v} {t.1}")
}
fn user_enum(o: E) {
    let t: (W1, i64) = match o { E.A(w) => (w, 1), E.B => (mk(0), 0) };
    println(f"ue{t.0.v} {t.1}")
}
fn result_tail(o: Result[W1, i64]) {
    let t: (W1, i64) = match o { Ok(w) => (w, 1), Err(_) => (mk(0), 0) };
    println(f"rs{t.0.v} {t.1}")
}
fn if_let_tail(o: Option[W1]) {
    let t: (W1, i64) = if let Some(w) = o { (w, 1) } else { (mk(0), 0) };
    println(f"il{t.0.v} {t.1}")
}
fn if_let_bare(o: Option[W1]) {
    let t = if let Some(w) = o { (w, 1) } else { (mk(0), 0) };
    println(f"ib{t.1}")
}
fn main() {
    bare_tuple(Some(mk(1)));
    bare_tuple(None);
    read_tuple(Some(mk(2)));
    struct_tail(Some(mk(3)));
    array_tail(4);
    braced_array(5);
    inline_payload(Some(W2 { v: 6 }));
    user_enum(E.A(mk(7)));
    result_tail(Ok(mk(8)));
    if_let_tail(Some(mk(9)));
    if_let_tail(None);
    if_let_bare(Some(mk(12)));
    let mut i = 10;
    while i < 12 {
        let o = Some(mk(i));
        let t: (W1, i64) = match o { Some(w) => (w, 1), None => (mk(0), 0) };
        println(f"lp{t.0.v}");
        i = i + 1;
    }
    println("end")
}
"#;
    let want = "bt1\ndW1_1\nbt0\ndW1_0\nrt2 1\ndW1_2\nst3 1\ndW1_3\nat4\ndW1_4\nba5\ndW1_5\nip6 1\ndW2_6\nue7 1\ndW1_7\nrs8 1\ndW1_8\nil9 1\ndW1_9\nil0 0\ndW1_0\nib1\ndW1_12\nlp10\ndW1_10\nlp11\ndW1_11\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
