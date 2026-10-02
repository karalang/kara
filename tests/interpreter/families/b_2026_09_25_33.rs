//! B-2026-09-25-33 -- a by-value param handed to a wrapper whose result is
//! DISCARDED runs its `Drop` body once, in the caller.

use super::*;

/// B-2026-09-25-33 — under `--interp` a param VIEW moved into a wrapper's
/// struct literal (`wrapC(x);` over `fn wrapC(v: R) -> BxP { BxP { v: v } }`)
/// ran its body when the discarded result died and again in the caller. The
/// discard now masks the view's field out of its walk (design.md § Drop
/// ordering rule 3), as B-2026-10-01-25 does for a discarded literal. Same
/// program as the codegen twin.
#[test]
fn interp_discarded_wrapper_of_param_view_runs_body_once_in_caller() {
    let out = run(r#"struct R { id: i64, tag: String }
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
    outerC(mk(1)); println("x"); outerG(mk(2)); println("x"); outerL(mk(3)); println("x")
    outerT(mk(4)); println("x"); outerO(mk(6)); println("x"); outerF(mk(7)); println("x")
    wrapC(mk(8)); println("x")
    let a = mk(10); outerC(a); println("x")
    let b = mk(11); outerO(b); println("x")
    println("end")
}
"#);
    assert_eq!(out, "c\ndR1\nx\ng\ndR2\nx\nl\ndR3\nx\ndR9\nt\ndR4\nx\ndO\no\ndR6\nx\ndR5\nf\ndR7\nx\ndR8\nx\nc\ndR10\nx\ndO\no\ndR11\nx\nend\n");
}
