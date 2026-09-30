//! B-2026-09-30-57 -- a fresh element beside a moved local in a collection
//! literal argument runs its `Drop` body, and its heap is freed.

use super::*;

/// B-2026-09-30-57 — the memory half: a literal whose place is a `match`
/// payload COPY declined B-2026-09-30-34's whole-array free (it would free the
/// copy's heap under the scrutinee), which leaked every other item's heap --
/// the fresh item and a sole-owner place alike. Those are now freed item by
/// item. Same program as the codegen twin.
#[test]
fn asan_literal_arg_fresh_item_beside_place_frees_it() {
    assert_clean_asan_run(
        r#"struct W1 { v: i64, s: String }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
fn take2(x: Array[W1, 2]) -> i64 { return x[0].v + x[1].v }
fn take3(x: Array[W1, 3]) -> i64 { return x[1].v }
fn takev(x: Vec[W1]) -> i64 { return x[0].v }
fn pass2(x: Array[W1, 2]) -> Array[W1, 2] { return x }
struct K { n: i64 }
impl K { fn take(ref self, x: Array[W1, 2]) -> i64 { return x[0].v + x[1].v } }
fn s(n: i64) -> String { return f"ssssssssssssssssssssssssssssss{n}" }
fn main() {
    let a = W1 { v: 1, s: s(1) };
    let r1 = take2([a, W1 { v: 2, s: s(2) }]);
    println(f"r1:{r1}");
    let b = W1 { v: 3, s: s(3) };
    let r2 = take3([W1 { v: 4, s: s(4) }, b, W1 { v: 5, s: s(5) }]);
    println(f"r2:{r2}");
    let c = W1 { v: 6, s: s(6) };
    let r3 = takev([c, W1 { v: 7, s: s(7) }]);
    println(f"r3:{r3}");
    let d = W1 { v: 8, s: s(8) };
    let r4 = takev(Vec[W1 { v: 9, s: s(9) }, d]);
    println(f"r4:{r4}");
    let k = K { n: 0 };
    let e = W1 { v: 10, s: s(10) };
    let r5 = k.take([e, W1 { v: 11, s: s(11) }]);
    println(f"r5:{r5}");
    let f = |x: Array[W1, 2]| x[0].v;
    let g = W1 { v: 12, s: s(12) };
    let r6 = f([g, W1 { v: 13, s: s(13) }]);
    println(f"r6:{r6}");
    let mut i = 0;
    while i < 2 { let w = W1 { v: 20 + i, s: s(i) }; let q = take2([W1 { v: 30 + i, s: s(i) }, w]); println(f"l:{q}"); i = i + 1; }
    let h = W1 { v: 14, s: s(14) };
    let r7 = pass2([h, W1 { v: 15, s: s(15) }]);
    println(f"r7:{r7[1].v}");
    let o = Some(W1 { v: 16, s: s(16) });
    match o { Some(x) => { let q = take2([x, W1 { v: 17, s: s(17) }]); println(f"m:{q}") }, None => {} }
    let m = W1 { v: 18, s: s(18) };
    let o2 = Some(W1 { v: 19, s: s(19) });
    match o2 { Some(x) => { let q = take3([x, W1 { v: 40, s: s(40) }, m]); println(f"m2:{q}") }, None => {} }
    println("end");
}
"#,
        &[
            "dW1_2", "dW1_1", "r1:3", "dW1_4", "dW1_5", "dW1_3", "r2:3", "dW1_7", "dW1_6", "r3:6",
            "dW1_9", "dW1_8", "r4:9", "dW1_11", "dW1_10", "r5:21", "dW1_13", "dW1_12", "r6:12",
            "dW1_30", "dW1_20", "l:50", "dW1_31", "dW1_21", "l:52", "r7:15", "dW1_14", "dW1_15",
            "dW1_17", "m:33", "dW1_16", "dW1_40", "m2:40", "dW1_19", "dW1_18", "end",
        ],
        "B-2026-09-30-57 fresh item beside a place in a literal arg",
    );
}
