//! B-2026-09-30-59 -- a discarded array literal that moves a local in runs
//! its body once and frees the local's heap.

use super::*;

/// B-2026-09-30-59 — `let w = W1 { .. }; let _ = [w];` lost `w`'s `String`
/// on every compiled surface (1 block): the discard predicate declined any
/// literal naming a place, so nothing owned the buffer `w` moved into. The
/// bare statement `[w];` also ran the body TWICE under `--interp`. Covers
/// `[..]`, `Array[..]`, `Vec[..]`, `let _ =` and the bare statement, a
/// mixed literal, two places, heap-only structs and a loop.
#[test]
fn e2e_discarded_array_literal_moving_a_local_runs_one_body() {
    let src = r#"struct W1 { v: i64, s: String }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct H1 { v: i64, s: String }
fn mk(n: i64) -> W1 { return W1 { v: n, s: f"ssssssssssssssssssssssssssssss{n}" } }
fn mh(n: i64) -> H1 { return H1 { v: n, s: f"hhhhhhhhhhhhhhhhhhhhhhhhhhhhhh{n}" } }
fn run() {
    let a = mk(1);
    let _ = [a];
    println("s1");
    let b = mk(2);
    let _ = Array[b];
    println("s2");
    let c = mk(3);
    [c];
    println("s3");
    let d = mk(4);
    let _ = Array[d, mk(5)];
    println("s4");
    let e = mk(6);
    let f = mk(7);
    [e, f];
    println("s5");
    let g = mk(8);
    let _ = Vec[g];
    println("s6");
    let h = mh(9);
    let _ = [h];
    let k = mh(10);
    Array[k];
    println("s7");
}
fn main() {
    run();
    let mut i = 0;
    while i < 2 { let w = mk(20 + i); let _ = [w]; let x = mk(30 + i); Array[x, mk(40 + i)]; i = i + 1; }
    println("end")
}
"#;
    let want = "dW1_1\ns1\ndW1_2\ns2\ndW1_3\ns3\ndW1_4\ndW1_5\ns4\ndW1_6\ndW1_7\ns5\ndW1_8\ns6\ns7\ndW1_20\ndW1_30\ndW1_40\ndW1_21\ndW1_31\ndW1_41\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
