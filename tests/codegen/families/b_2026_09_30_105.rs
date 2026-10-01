//! B-2026-09-30-105 -- an `Option`/`Result` payload binding discarded or
//! moved inside a `match` arm or `if let`: its body runs once, where the
//! interpreter runs it, on every path.

use super::*;

/// B-2026-09-30-105 — `Some(w) => { let _ = w; .. }` and `let _ = [w]` ran the
/// body at arm end and leaked the payload compiled, against the statement under
/// `--interp`. Also covered: `[w];`, a mixed `[w, mk(49)]` (whose fresh element's
/// body ran nowhere), `Vec[w]`, an `Ok` payload, and the same discards plus
/// `let k = w` and `(w, 1)` NESTED in a branch of the arm, which ran no body on
/// the path that did not take them; and the loop shapes, where a per-path bit
/// cleared on one iteration silenced the next (and `if let` in the interpreter
/// kept a move record across iterations).
#[test]
fn e2e_payload_binding_discarded_in_arm_runs_body_at_statement() {
    let src = r#"struct W1 { v: i64, s: String }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
fn mk(n: i64) -> W1 { return W1 { v: n, s: f"ssssssssssssssssssssssssssssss{n}" } }
fn branch(c: bool, n: i64) {
    let o = Some(mk(n));
    match o { Some(w) => { if c { let _ = w; println("a") }; println("arm") }, None => {} };
    let p = Some(mk(n + 1));
    match p { Some(w) => { if c { let k = w; println(f"k:{k.v}") }; println("arm") }, None => {} };
    let q = Some(mk(n + 2));
    if let Some(w) = q { if c { [w]; println("b") }; println("arm") };
    let r = Some(mk(n + 3));
    match r { Some(w) => { if c { let _ = (w, 1); println("t") }; println("arm") }, None => {} };
}
fn main() {
    let o1 = Some(mk(1));
    match o1 { Some(w) => { let _ = w; println("arm1") }, None => {} };
    let o2 = Some(mk(2));
    match o2 { Some(w) => { let _ = [w]; println("arm2") }, None => {} };
    let o3 = Some(mk(3));
    if let Some(w) = o3 { [w]; println("arm3") };
    let o4 = Some(mk(4));
    match o4 { Some(w) => { let _ = [w, mk(49)]; println("arm4") }, None => {} };
    let o5 = Some(mk(5));
    match o5 { Some(w) => { let _ = Vec[w]; println("arm5") }, None => {} };
    let r6: Result[W1, i64] = Ok(mk(6));
    match r6 { Ok(w) => { let _ = Array[w]; println("arm6") }, Err(e) => { println(f"{e}") } };
    branch(false, 10);
    branch(true, 20);
    for i in 0..3 {
        let o = Some(mk(30 + i));
        if let Some(w) = o { if i < 1 { let _ = w; println("in") } };
        println("it")
    };
    let mut j = 0;
    while j < 3 {
        let o = Some(mk(40 + j));
        match o { Some(w) => { if j % 2 == 0 { let k = w; println(f"k:{k.v}") } }, None => {} };
        j = j + 1;
    };
    println("end")
}
"#;
    let want = "dW1_1\narm1\ndW1_2\narm2\ndW1_3\narm3\ndW1_4\ndW1_49\narm4\ndW1_5\narm5\ndW1_6\narm6\narm\ndW1_10\narm\ndW1_11\narm\ndW1_12\narm\ndW1_13\ndW1_20\na\narm\nk:21\ndW1_21\narm\ndW1_22\nb\narm\ndW1_23\nt\narm\ndW1_30\nin\nit\ndW1_31\nit\ndW1_32\nit\nk:40\ndW1_40\ndW1_41\nk:42\ndW1_42\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
