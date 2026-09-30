//! B-2026-09-30-33 -- a fresh owned `Vec` argument to a CLOSURE call is freed
//! by the caller, as it is for a free-function call.

use super::*;

/// B-2026-09-30-33 — a collection literal (`f([1, 2])`, `f(Vec[8])`,
/// `f([0; 4])`), a single-tail block (`f({ [4] })`) and a branching arg whose
/// every tail is a call (`f(if c { mk(1) } else { mk(2) })`) each leaked one
/// buffer per closure call on every compiled surface, while the same argument
/// to a free function was clean. The block cells with a tail that hands out a
/// binding (outer `{ loc }`, local `{ let t = ..; t }`) were already owned and
/// guard against a second free.
#[test]
fn asan_closure_fresh_vec_arg_is_freed_by_caller() {
    assert_clean_asan_run(
        r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct W2 { v: i64, s: String }
impl Drop for W2 { fn drop(mut ref self) { println(f"dW2_{self.v}") } }
fn mk(n: i64) -> Vec[i64] { return [n, n] }
fn main() {
    let f = |x: Vec[i64]| x.len();
    let s = |t: String| t.len();
    let a = f([1, 2]);
    let b = f(Vec[8]);
    let q = f([0; 4]);
    let e = f({ [4] });
    let cnd = true;
    let r = f(if cnd { mk(1) } else { mk(2) });
    let g = [f];
    let z = g[0]([1, 2, 3]);
    println(f"{a} {b} {q} {e} {r} {z}");
    let loc = [7, 8, 9];
    let h = f({ loc });
    let k = f({ let t = mk(3); t });
    let nm = "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz".to_string();
    let d = s({ nm });
    println(f"{h} {k} {d}");
    let fw = |x: Vec[W1]| x.len();
    let n1 = fw([W1 { v: 1 }, W1 { v: 2 }]);
    let fw2 = |x: Vec[W2]| x.len();
    let n2 = fw2([W2 { v: 3, s: f"sssssssssssssssssssssssssss{3}" }]);
    let fs = |x: Vec[String]| x.len();
    let n3 = fs([f"aaaaaaaaaaaaaaaaaaaaaaaaaaaaa{1}", "b".to_string()]);
    let id = |x: Vec[i64]| x;
    let w = id([4, 5]);
    println(f"{n1} {n2} {n3} {w.len()}");
    let mut i = 0;
    while i < 3 { let m = f([i, i]); let m2 = fs(Vec[f"x{i}"]); println(f"{m} {m2}"); i = i + 1; }
    println("end");
}
"#,
        &[
            "2 1 4 1 2 3",
            "3 2 32",
            "dW1_1",
            "dW1_2",
            "dW2_3",
            "2 1 2 2",
            "2 1",
            "2 1",
            "2 1",
            "end",
        ],
        "B-2026-09-30-33 closure fresh Vec arg",
    );
}
