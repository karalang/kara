//! B-2026-09-30-57 -- a fresh element beside a moved local in a collection
//! literal argument runs its `Drop` body, and its heap is freed.

use super::*;

/// B-2026-09-30-57 — `take2([a, W1 { v: 2, .. }])` printed `dW1_1 r1:3 end`
/// on all four surfaces: the literal holds a place, so the fresh-literal walk
/// declined it, and the fresh `dW1_2` never ran. Covers `Array` and `Vec`
/// params (bare and `Vec[..]` literals), the place first, last and in the
/// middle, a method, a closure, a loop, a `match` payload copy as the place,
/// and a payload copy beside a sole-owner place; the hand-back `pass2(..)`
/// keeps its bodies on the result.
#[test]
fn e2e_literal_arg_fresh_item_beside_place_runs_body() {
    let src = r#"struct W1 { v: i64, s: String }
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
"#;
    let want = "dW1_2\ndW1_1\nr1:3\ndW1_4\ndW1_5\ndW1_3\nr2:3\ndW1_7\ndW1_6\nr3:6\ndW1_9\ndW1_8\nr4:9\ndW1_11\ndW1_10\nr5:21\ndW1_13\ndW1_12\nr6:12\ndW1_30\ndW1_20\nl:50\ndW1_31\ndW1_21\nl:52\nr7:15\ndW1_14\ndW1_15\ndW1_17\nm:33\ndW1_16\ndW1_40\nm2:40\ndW1_19\ndW1_18\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
