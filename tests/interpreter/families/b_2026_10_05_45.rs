//! B-2026-10-05-45: generic callee rebinding one param's name to another param

use super::*;

/// B-2026-10-05-45: a GENERIC callee that rebinds one by-value param's name to
/// another param before anything reads it (`fn g5[T](s: T, o: T) -> T { let s
/// = o; s }`). The name-keyed alias walks count `s` as bound twice, the param and
/// the `let`, so `s` was never an alias of `o`: the caller kept `o`'s body while
/// the result owned it too, and the shadowed param's own body was lost. A param
/// shadowed before any use now has no alias but itself, and its shadowing local
/// counts as bound once. Cells cover temporary and named arguments in both
/// orders, `let mut`, a branch return, a further rebind, `Option` params, a
/// discarded call, and the non-generic twin.
#[test]
fn interp_generic_param_rebound_to_another_param_runs_each_body_once() {
    let out = run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
fn g5[T](s: T, o: T) -> T { let s = o; s }
fn gm[T](s: T, o: T) -> T { let mut s = o; s }
fn gb[T](s: T, o: T, c: bool) -> T { let s = o; if c { return s } s }
fn gc[T](s: T, o: T) -> T { let s = o; let t = s; t }
fn go[T](s: Option[T], o: Option[T]) -> Option[T] { let s = o; s }
fn n5(s: R, o: R) -> R { let s = o; s }
fn main() {
    let r = g5(R { id: 1 }, R { id: 2 }); println(f"k{r.id}"); println("_a1")
    let a = R { id: 3 }; let b = R { id: 4 }; let r = g5(a, b); println(f"k{r.id}"); println("_a2")
    let a = R { id: 5 }; let b = R { id: 6 }; let r = g5(b, a); println(f"k{r.id}"); println("_a3")
    let r = gm(R { id: 7 }, R { id: 8 }); println(f"k{r.id}"); println("_a4")
    let r = gb(R { id: 9 }, R { id: 10 }, true); println(f"k{r.id}"); println("_a5")
    let r = gc(R { id: 11 }, R { id: 12 }); println(f"k{r.id}"); println("_a6")
    let r = go(Some(R { id: 13 }), Some(R { id: 14 })); println(f"k{r.is_some()}"); println("_a7")
    g5(R { id: 15 }, R { id: 16 }); println("_a8")
    let r = n5(R { id: 17 }, R { id: 18 }); println(f"k{r.id}"); println("_a9")
    println("end")
}
"#);
    assert_eq!(out, "d1\nk2\nd2\n_a1\nd3\nk4\nd4\n_a2\nd6\nk5\nd5\n_a3\nd7\nk8\nd8\n_a4\nd9\nk10\nd10\n_a5\nd11\nk12\nd12\n_a6\nd13\nktrue\nd14\n_a7\nd15\nd16\n_a8\nd17\nk18\nd18\n_a9\nend\n", "got:\n{out}");
}
