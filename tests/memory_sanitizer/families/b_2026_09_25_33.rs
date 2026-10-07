//! B-2026-09-25-33 -- a by-value param handed to a wrapper whose result is
//! DISCARDED runs its `Drop` body once, in the caller.

use super::*;

/// B-2026-09-25-33 — the memory half: the discarded wrapper result still owns
/// the callee's entry copy of the view it holds, so the masked field's memory
/// is freed with it (the generic wrapper leaked it before). Same program as the
/// codegen twin.
#[test]
fn asan_discarded_wrapper_of_param_view_freed_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct BxP { v: R }
struct Bx[T] { v: T }
struct Two { a: R, b: R }
struct OwnD { v: R }
impl Drop for OwnD { fn drop(mut ref self) { println("dO") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"tttttttttttttttttttttttttttttt{i}" } }
fn wrapC(v: R) -> BxP { return BxP { v: v } }
fn wrap[T](v: T) -> Bx[T] { return Bx { v: v } }
fn two(a: R, b: R) -> Two { Two { a: a, b: b } }
fn ownd(v: R) -> OwnD { return OwnD { v: v } }
fn outerC(x: R) { wrapC(x); println("c") }
fn outerG[T](x: T) { wrap(x); println("g") }
fn outerL(x: R) { let b = wrapC(x); println("l") }
fn outerT(x: R) { two(x, mk(9)); println("t") }
fn outerO(x: R) { ownd(x); println("o") }
fn outerF(x: R) { wrapC(mk(5)); println("f") }
fn main() {
    outerC(mk(1)); println("x"); outerG(mk(2)); println("x"); outerL(mk(3)); println("x");
    outerT(mk(4)); println("x"); outerO(mk(6)); println("x"); outerF(mk(7)); println("x");
    wrapC(mk(8)); println("x");
    let a = mk(10); outerC(a); println("x");
    let b = mk(11); outerO(b); println("x");
    println("end")
}
"#,
        &[
            "c", "dR1", "x", "g", "dR2", "x", "l", "dR3", "x", "dR9", "t", "dR4", "x", "dO", "o",
            "dR6", "x", "dR5", "f", "dR7", "x", "dR8", "x", "c", "dR10", "x", "dO", "o", "dR11",
            "x", "end",
        ],
        "discarded_wrapper_of_param_view",
    );
}
