//! B-2026-10-06-63 — a container moved out on one path and then reassigned walks no freed buffer.

use super::*;

/// B-2026-10-06-63 — a `Vec` (or fixed `Array`) local moved out on ONE path
/// (`if`, `else`, a `match` arm, one loop iteration) and then reassigned runs
/// the displaced value's `Drop` bodies only on the paths that still owned it,
/// and never walks the buffer the moving binding already freed (`a`, `b`,
/// `d`, `e`, `g`, `h`, `k`, each with the move taken and not taken).
///
/// Before: the store's displaced-element walk ignored the per-path move bit
/// and walked the moved header (its capacity zeroed, its length intact), so
/// on the moving path it read freed memory and ran a garbage body.
#[test]
fn asan_cond_moved_vec_reassign_skips_freed_elems() {
    assert_clean_asan_run_min_allocs(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn a(c: bool) { let mut v = [R { id: 1 }]; if c { let q = v; println(f"q{q.len()}"); } v = [R { id: 2 }]; println(f"a{v.len()}") }
fn b(c: bool) { let mut v = [R { id: 3 }]; if c { println("t"); } else { let q = v; println(f"q{q.len()}"); } v = [R { id: 4 }]; println(f"b{v.len()}") }
fn d(n: i64) { let mut v = [R { id: 5 }]; let mut i = 0; while i < n { if i == 1 { let q = v; println(f"q{q.len()}"); } v = [R { id: 6 + i }]; i = i + 1; } println(f"d{v.len()}") }
fn e(c: bool) { let mut v: Vec[Vec[R]] = [[R { id: 10 }]]; if c { let q = v; println(f"q{q.len()}"); } v = [[R { id: 11 }]]; println(f"e{v.len()}") }
fn f(c: bool) { let mut v = [R { id: 12 }]; if c { let q = v; println(f"q{q.len()}"); } v = [R { id: 13 }]; v = [R { id: 14 }]; println(f"f{v.len()}") }
fn g(c: bool) { let mut v = [R { id: 15 }]; match c { true => { let q = v; println(f"q{q.len()}"); } false => { println("n"); } } v = [R { id: 16 }]; println(f"g{v.len()}") }
fn h(c: bool) { let mut v = [R { id: 17 }]; if c { let q = v; println(f"q{q.len()}"); } let w = [R { id: 18 }]; v = w; println(f"h{v.len()}") }
fn k(c: bool) { let mut a = [R { id: 19 }, R { id: 20 }]; if c { let q = a; println(f"q{q.len()}"); } a = [R { id: 21 }, R { id: 22 }]; println(f"k{a.len()}") }
fn main() {
    a(true);
    a(false);
    b(true);
    b(false);
    d(3);
    e(true);
    e(false);
    f(true);
    g(true);
    g(false);
    h(true);
    h(false);
    k(true);
    k(false);
    println("end")
}"#,
        &[
            "q1", "dR1", "a1", "dR2", "dR1", "a1", "dR2", "t", "dR3", "b1", "dR4", "q1", "dR3",
            "b1", "dR4", "dR5", "q1", "dR6", "dR7", "d1", "dR8", "q1", "dR10", "e1", "dR11",
            "dR10", "e1", "dR11", "q1", "dR12", "dR13", "f1", "dR14", "q1", "dR15", "g1", "dR16",
            "n", "dR15", "g1", "dR16", "q1", "dR17", "h1", "dR18", "dR17", "h1", "dR18", "q2",
            "dR19", "dR20", "k2", "dR21", "dR22", "dR19", "dR20", "k2", "dR21", "dR22", "end",
        ],
        "asan_cond_moved_vec_reassign_skips_freed_elems",
        8,
    );
}
