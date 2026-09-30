//! B-2026-09-30-56 -- a named local moved into a collection literal argument
//! that the callee hands back or stores runs its `Drop` body once.

use super::*;

/// B-2026-09-30-56 — `let r = pass([w])` over `fn pass(x: Array[W1, 1]) ->
/// Array[W1, 1] { return x }` printed `dW1_40 r:40 dW1_40` on all four
/// surfaces: `w`'s own binding ran the body after the call and `r` ran it
/// again. Covers a `Vec` literal, a conditional hand-back on both legs, a
/// store into a `mut ref` container and into `self`, a discarded result, a
/// method, a generic callee, two places, a place beside a fresh element, a
/// loop, and an element that owns no heap; the non-escaping `take([w])`
/// keeps its one body at the binding.
#[test]
fn e2e_literal_arg_local_handed_back_runs_body_once() {
    let src = r#"struct W1 { v: i64, s: String }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct P { v: i64 }
impl Drop for P { fn drop(mut ref self) { println(f"dP{self.v}") } }
fn pass(x: Array[W1, 1]) -> Array[W1, 1] { return x }
fn passv(x: Vec[W1]) -> Vec[W1] { return x }
fn take(x: Array[W1, 1]) -> i64 { return x[0].v }
fn p2(x: Array[W1, 2]) -> Array[W1, 2] { return x }
fn pc(x: Array[W1, 1], c: bool) -> Option[Array[W1, 1]] { if c { return Some(x) } return None }
fn keep(v: mut ref Vec[Array[W1, 1]], x: Array[W1, 1]) { v.push(x) }
fn gp[T](x: T) -> T { return x }
fn pp(x: Array[P, 1]) -> Array[P, 1] { return x }
struct K { n: i64 }
impl K { fn pm(ref self, x: Array[W1, 1]) -> Array[W1, 1] { return x } }
struct Bag { xs: Vec[Array[W1, 1]] }
impl Bag { fn put(mut ref self, x: Array[W1, 1]) { self.xs.push(x) } }
fn s(n: i64) -> String { return f"ssssssssssssssssssssssssssssss{n}" }
fn main() {
    let w1 = W1 { v: 1, s: s(1) };
    let r1 = pass([w1]);
    println(f"r1:{r1[0].v}");
    let w2 = W1 { v: 2, s: s(2) };
    let r2 = passv([w2]);
    println(f"r2:{r2[0].v}");
    let w3 = W1 { v: 3, s: s(3) };
    let r3 = pc([w3], true);
    println("a");
    let w4 = W1 { v: 4, s: s(4) };
    let r4 = pc([w4], false);
    println("b");
    let mut v: Vec[Array[W1, 1]] = [];
    let w5 = W1 { v: 5, s: s(5) };
    keep(mut v, [w5]);
    println(f"c:{v.len()}");
    let w6 = W1 { v: 6, s: s(6) };
    pass([w6]);
    println("d");
    let k = K { n: 0 };
    let w7 = W1 { v: 7, s: s(7) };
    let r7 = k.pm([w7]);
    println(f"r7:{r7[0].v}");
    let w8 = W1 { v: 8, s: s(8) };
    let r8 = gp([w8]);
    println(f"r8:{r8[0].v}");
    let a9 = W1 { v: 9, s: s(9) };
    let b9 = W1 { v: 10, s: s(10) };
    let r9 = p2([a9, b9]);
    println(f"r9:{r9[1].v}");
    let c11 = W1 { v: 11, s: s(11) };
    let r11 = p2([c11, W1 { v: 12, s: s(12) }]);
    println(f"r11:{r11[0].v}");
    let mut i = 0;
    while i < 2 { let w = W1 { v: 20 + i, s: s(i) }; let r = pass([w]); println(f"l:{r[0].v}"); i = i + 1; }
    let p = P { v: 13 };
    let rp = pp([p]);
    println(f"rp:{rp[0].v}");
    let mut bag = Bag { xs: [] };
    let w14 = W1 { v: 14, s: s(14) };
    bag.put([w14]);
    println(f"bag:{bag.xs.len()}");
    let w15 = W1 { v: 15, s: s(15) };
    let t = take([w15]);
    println(f"t:{t}");
    println("end");
}
"#;
    let want = "r1:1\ndW1_1\nr2:2\ndW1_2\ndW1_3\na\ndW1_4\nb\nc:1\ndW1_5\ndW1_6\nd\nr7:7\ndW1_7\nr8:8\ndW1_8\nr9:10\ndW1_9\ndW1_10\nr11:11\ndW1_11\ndW1_12\nl:20\ndW1_20\nl:21\ndW1_21\nrp:13\ndP13\nbag:1\ndW1_14\ndW1_15\nt:15\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
