//! B-2026-09-30-31 -- a discarded heap-free `Drop` aggregate runs its bodies
//! once and touches no memory it does not own.

use super::*;

/// B-2026-09-30-31 — the bodies-only registration for a discarded aggregate
/// the memory walk declines runs each body once, with no double free.
#[test]
fn asan_discarded_heap_free_drop_aggregate_runs_bodies_once() {
    assert_clean_asan_run(
        r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
fn mka() -> Array[W1, 1] { return [W1 { v: 40 }] }
fn mkv() -> Vec[W1] { return [W1 { v: 41 }] }
fn mkt() -> (W1, i64) { return (W1 { v: 42 }, 7) }
fn mk2() -> ((W1, W1), i64) { return ((W1 { v: 43 }, W1 { v: 44 }), 1) }
fn pa(x: Array[W1, 1]) -> Array[W1, 1] { return x }
fn pt(x: (W1, i64)) -> (W1, i64) { return x }
struct K { n: i64 }
impl K { fn ma(ref self) -> Array[W1, 1] { return [W1 { v: 45 }] } fn mt(ref self) -> (W1, i64) { return (W1 { v: 46 }, 1) } }
fn main() {
    mka();
    println("a");
    mkt();
    println("b");
    let _ = mka();
    println("c");
    let _ = mkt();
    println("d");
    mk2();
    println("e");
    let a: Array[W1, 1] = [W1 { v: 1 }];
    pa(a);
    println("f");
    let t = (W1 { v: 2 }, 3);
    pt(t);
    println("g");
    pa([W1 { v: 4 }]);
    println("h");
    let k = K { n: 0 };
    k.ma();
    k.mt();
    println("j");
    let c = true;
    if c { mka() } else { mka() };
    println("k");
    let m = 2;
    match m { 1 => mkt(), _ => mkt() };
    println("l");
    let mut i = 0;
    while i < 2 { mka(); i = i + 1; }
    println("end")
}
"#,
        &[
            "dW1_40", "a", "dW1_42", "b", "dW1_40", "c", "dW1_42", "d", "dW1_43", "dW1_44", "e",
            "dW1_1", "f", "dW1_2", "g", "dW1_4", "h", "dW1_45", "dW1_46", "j", "dW1_40", "k",
            "dW1_42", "l", "dW1_40", "dW1_40", "end",
        ],
        "B-2026-09-30-31 discarded heap-free Drop aggregate",
    );
}
