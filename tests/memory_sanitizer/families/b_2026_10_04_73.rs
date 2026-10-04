//! B-2026-10-04-73 — reassigning a tuple element that has moved out runs the
//! new value's `Drop` body.

use super::*;

/// `let x = t.0; t.0 = mk(2);` left the element masked out of the tuple's walk,
/// so the replacement's body ran nowhere, on every surface. Covers the
/// unconditional move then store, a move on one path then store, a move and
/// store on the same path, an unconditional move then a store one frame deeper
/// (taken and not taken), a store in a loop after the move (each pass displaces
/// the last), two stores after a move, a struct element with a `Drop` field,
/// and `Option` / user-enum elements.
#[test]
fn asan_reassigned_moved_out_tuple_elem_runs_new_body() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct P { r: R, k: i64 }
enum E { A(R), B }
fn mk(i: i64) -> R { R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn plain() { let mut t: (R, i64) = (mk(1), 0); let x = t.0; t.0 = mk(2); println(f"x{x.id}"); println(f"a{t.1}") }
fn movedthen(c: bool) { let mut t: (R, i64) = (mk(3), 0); if c { let x = t.0; println(f"x{x.id}"); } t.0 = mk(4); println(f"b{t.1}") }
fn samepath(c: bool) { let mut t: (R, i64) = (mk(5), 0); if c { let x = t.0; println(f"x{x.id}"); t.0 = mk(6); } println(f"c{t.1}") }
fn deeper(c: bool) { let mut t: (R, i64) = (mk(7), 0); let x = t.0; println(f"x{x.id}"); if c { t.0 = mk(8); } println(f"d{t.1}") }
fn inloop() { let mut t: (R, i64) = (mk(9), 0); let x = t.0; println(f"x{x.id}"); for i in 10..13 { t.0 = mk(i); } println(f"e{t.1}") }
fn twice() { let mut t: (R, R) = (mk(14), mk(15)); let x = t.0; t.0 = mk(16); t.0 = mk(17); println(f"x{x.id} f{t.1.id}") }
fn nested() { let mut t: (P, i64) = (P { r: mk(18), k: 1 }, 0); let x = t.0; println(f"x{x.k}"); t.0 = P { r: mk(19), k: 2 }; println(f"g{t.1}") }
fn opt() { let mut t: (Option[R], i64) = (Some(mk(20)), 0); let x = t.0; t.0 = Some(mk(21)); println(f"h{t.1}") }
fn en() { let mut t: (E, i64) = (E.A(mk(22)), 0); let x = t.0; t.0 = E.A(mk(23)); println(f"i{t.1}") }
fn main() {
    plain();
    movedthen(true); movedthen(false);
    samepath(true); samepath(false);
    deeper(true); deeper(false);
    inloop(); twice(); nested(); opt(); en();
    println("end");
}
"#,
        &[
            "x1", "dR1", "a0", "dR2", "x3", "dR3", "b0", "dR4", "dR3", "b0", "dR4", "x5", "dR5",
            "c0", "dR6", "c0", "dR5", "x7", "dR7", "d0", "dR8", "x7", "dR7", "d0", "x9", "dR9",
            "dR10", "dR11", "e0", "dR12", "dR16", "x14 f15", "dR14", "dR17", "dR15", "x1", "dR18",
            "g0", "dR19", "dR20", "h0", "dR21", "dR22", "i0", "dR23", "end",
        ],
        "reassigned_moved_out_tuple_elem",
    );
}
