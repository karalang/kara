//! B-2026-10-03-21: per-call ownership bookkeeping skips scalar arguments

use super::*;

/// B-2026-10-03-21: the interpreter now skips its callee-body ownership walks
/// for an argument holding a bare scalar, which has no body to disarm and no
/// part to mask. A scalar PROJECTED out of a fresh temp (`f(mk(1).id)`,
/// `f(mkh(3).r.id)`) still runs the temp's `Drop` body at the end of the
/// statement, and a `Drop`-typed argument beside a scalar one (`g(k, mk(7))`)
/// still dies in the callee. Pins the behaviour the skip must not change.
#[test]
fn interp_scalar_args_beside_drop_temps_keep_their_bodies() {
    let out = run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct H { r: R, n: i64 }
fn mk(i: i64) -> R { return R { id: i } }
fn mkh(i: i64) -> H { return H { r: R { id: i }, n: i } }
fn f(a: i64) -> i64 { println(f"f{a}"); return a }
fn g(a: i64, r: R) -> i64 { return a + r.id }
fn main() {
    let x = f(mk(1).id);
    let y = f(mkh(2).n);
    let z = f(mkh(3).r.id);
    let w = g(f(4), mk(5));
    let k = 6;
    let q = g(k, mk(7));
    println(f"{x} {y} {z} {w} {q}");
}
"#);
    assert_eq!(out, "f1\ndR1\nf2\ndR2\nf3\ndR3\nf4\ndR5\ndR7\n1 2 3 9 13\n");
}
