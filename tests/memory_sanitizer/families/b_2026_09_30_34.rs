//! B-2026-09-30-34 -- an `Array` literal argument holding a moved local frees
//! that element's heap once.

use super::*;

/// B-2026-09-30-34 — `let w = W1 { .. }; take([w])` over a caller-retained
/// `Array[W1, N]` param (W1 runs a user `Drop` and owns a `String`) leaked the
/// element's heap on every compiled surface: the move zeroed `w`'s fields, so
/// its binding (which still runs the body) frees nothing, and the callee
/// frees nothing either. Covers a statement call, a method, a closure, two
/// places, a loop, and `Vec` / `Option` / nested-struct fields. The
/// `Some(w) => take([w])` cell is a guard: that `w` is a copy the scrutinee
/// still frees, and freeing it from the literal too was a double free.
#[test]
fn asan_array_literal_arg_with_moved_local_frees_it_once() {
    assert_clean_asan_run(
        r#"struct W1 { v: i64, s: String }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct In { t: String }
struct W3 { v: i64, q: Vec[String], o: Option[String], i: In }
impl Drop for W3 { fn drop(mut ref self) { println(f"dW3_{self.v}") } }
fn take(x: Array[W1, 1]) -> i64 { return x[0].v }
fn take2(x: Array[W1, 2]) -> i64 { return x[0].v + x[1].v }
fn t3(x: Array[W3, 1]) -> i64 { return x[0].v }
struct K { n: i64 }
impl K { fn take(ref self, x: Array[W1, 1]) -> i64 { return x[0].v } }
fn main() {
    let a = W1 { v: 1, s: f"ssssssssssssssssssssssssssssss{1}" };
    let r = take([a]);
    println(f"r:{r}");
    let b = W1 { v: 2, s: f"ssssssssssssssssssssssssssssss{2}" };
    take([b]);
    let k = K { n: 0 };
    let c = W1 { v: 3, s: f"ssssssssssssssssssssssssssssss{3}" };
    let r3 = k.take([c]);
    let f = |x: Array[W1, 1]| x[0].v;
    let d = W1 { v: 4, s: f"ssssssssssssssssssssssssssssss{4}" };
    let r4 = f([d]);
    let e = W1 { v: 5, s: f"ssssssssssssssssssssssssssssss{5}" };
    let g = W1 { v: 6, s: f"ssssssssssssssssssssssssssssss{6}" };
    let r5 = take2([e, g]);
    println(f"{r3} {r4} {r5}");
    let mut i = 0;
    while i < 2 { let w = W1 { v: 10 + i, s: f"ssssssssssssssssssssssssssssss{i}" }; let q = take([w]); println(f"q:{q}"); i = i + 1; }
    let h = W3 { v: 7, q: [f"qqqqqqqqqqqqqqqqqqqqqqqqqqqqqq{7}"], o: Some(f"oooooooooooooooooooooooooooooo{7}"), i: In { t: f"iiiiiiiiiiiiiiiiiiiiiiiiiiiiiii{7}" } };
    let r7 = t3([h]);
    println(f"r7:{r7}");
    let o = Some(W1 { v: 8, s: f"ssssssssssssssssssssssssssssss{8}" });
    match o { Some(w) => { let q = take([w]); println(f"m:{q}") }, None => {} }
    println("end");
}
"#,
        &[
            "dW1_1", "r:1", "dW1_2", "dW1_3", "dW1_4", "dW1_6", "dW1_5", "3 4 11", "dW1_10",
            "q:10", "dW1_11", "q:11", "dW3_7", "r7:7", "m:8", "dW1_8", "end",
        ],
        "B-2026-09-30-34 array literal arg with a moved local",
    );
}
