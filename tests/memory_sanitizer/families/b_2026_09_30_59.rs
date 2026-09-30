//! B-2026-09-30-59 -- a discarded array literal that moves a local in runs
//! its body once and frees the local's heap.

use super::*;

/// B-2026-09-30-59 — the memory half: the buffer (or `[N x T]` value) a
/// discarded array literal moved a local into had no owner, so the local's
/// `String` leaked. Same program as the codegen twin.
#[test]
fn asan_discarded_array_literal_moving_a_local_frees_it() {
    assert_clean_asan_run(
        r#"struct W1 { v: i64, s: String }
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
"#,
        &[
            "dW1_1", "s1", "dW1_2", "s2", "dW1_3", "s3", "dW1_4", "dW1_5", "s4", "dW1_6", "dW1_7",
            "s5", "dW1_8", "s6", "s7", "dW1_20", "dW1_30", "dW1_40", "dW1_21", "dW1_31", "dW1_41",
            "end",
        ],
        "discarded_array_literal_moving_a_local",
    );
}
