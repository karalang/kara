//! B-2026-09-30-60 -- a discarded `Array[..]` literal runs each element's
//! `Drop` body and frees its heap.

use super::*;

/// B-2026-09-30-60 — the memory half: nothing owned a discarded `Array[..]`
/// literal's `[N x T]` value, so every element's heap leaked. Same program as
/// the codegen twin.
#[test]
fn asan_discarded_fixed_array_literal_frees_elements() {
    assert_clean_asan_run(
        r#"struct W1 { v: i64, s: String }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct P { s: String }
fn mk(n: i64) -> W1 { return W1 { v: n, s: f"ssssssssssssssssssssssssssssss{n}" } }
fn run(c: bool) {
    let _ = Array[W1 { v: 1, s: f"a{1}" }];
    println("s1");
    Array[mk(2), mk(3)];
    println("s2");
    let _ = if c { Array[mk(4)] } else { Array[mk(5)] };
    println("s3");
    match c { true => Array[mk(6)], false => Array[mk(7)] };
    println("s4");
    let _ = { Array[mk(8)] };
    println("s5");
    let _ = Array[P { s: f"p{1}" }, P { s: f"p{2}" }];
    let _ = Array[f"x{1}", f"y{2}"];
    let _ = Array[1, 2];
    println("s6");
}
fn main() {
    run(true);
    run(false);
    let mut i = 0;
    while i < 2 { let _ = Array[mk(10 + i)]; Array[mk(20 + i)]; i = i + 1; }
    println("end")
}
"#,
        &[
            "dW1_1", "s1", "dW1_2", "dW1_3", "s2", "dW1_4", "s3", "dW1_6", "s4", "dW1_8", "s5",
            "s6", "dW1_1", "s1", "dW1_2", "dW1_3", "s2", "dW1_5", "s3", "dW1_7", "s4", "dW1_8",
            "s5", "s6", "dW1_10", "dW1_20", "dW1_11", "dW1_21", "end",
        ],
        "discarded_fixed_array_literal",
    );
}
