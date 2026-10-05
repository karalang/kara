//! B-2026-10-05-77: generic callee reading a param, then rebinding its name

use super::*;

/// B-2026-10-05-77: a callee that READS a by-value param and only then rebinds the
/// param's NAME with a `let` (`fn g0[T](s: T, o: T, c: bool) -> T { if c { return s }
/// let s = o; s }`). One name spelled two bindings, and the predicates both
/// backends use to decide who runs a param's `Drop` body are keyed by name, so
/// the tail `s` read as the param handed back: `d25 k25 d25` where `d24 k25 d25`
/// is due. Lowering now gives such a `let` a fresh name. Cells cover the row's
/// two legs, a further alias, a shadow inside a branch, a non-generic callee
/// returning a struct, a shadow that only reads the param first, a closure, an
/// `Option` param and a generic method.
#[test]
fn interp_generic_param_read_then_name_rebound_runs_each_body_once() {
    let out = run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct P { s: R, n: i64 }
fn g0[T](s: T, o: T, c: bool) -> T { if c { return s } let s = o; s }
fn g1[T](s: T, o: T, c: bool) -> T { if c { return s } let s = o; let p = s; p }
fn g2[T](s: T, o: T, c: bool) -> T { if c { let s = o; return s } s }
fn g3(s: R, o: R, c: bool) -> P { if c { return P { s, n: 1 } } let s = o; P { s, n: 2 } }
fn g4(s: R, c: bool) -> i64 { let k = s.id; let s = R { id: k + 100 }; if c { println(f"in{s.id}") } k }
fn g5[T](s: T, o: T, c: bool) -> T { if c { return s } let s = o; let f = || println("cl"); f(); s }
fn g6(s: Option[R], o: Option[R]) -> Option[R] { let n = s.is_some(); let s = o; if n { return s } None }
struct B { v: i64 }
impl B { fn m[T](self, s: T, o: T, c: bool) -> T { if c { return s } let s = o; s } }
fn main() {
    let r = g0(R { id: 24 }, R { id: 25 }, false); println(f"k{r.id}"); println("_a0")
    let r = g0(R { id: 22 }, R { id: 23 }, true); println(f"k{r.id}"); println("_b0")
    let r = g1(R { id: 1 }, R { id: 2 }, false); println(f"k{r.id}"); println("_a1")
    let r = g1(R { id: 3 }, R { id: 4 }, true); println(f"k{r.id}"); println("_a2")
    let r = g2(R { id: 5 }, R { id: 6 }, true); println(f"k{r.id}"); println("_a3")
    let r = g2(R { id: 7 }, R { id: 8 }, false); println(f"k{r.id}"); println("_a4")
    let p = g3(R { id: 9 }, R { id: 10 }, false); println(f"k{p.s.id}{p.n}"); println("_a5")
    let p = g3(R { id: 11 }, R { id: 12 }, true); println(f"k{p.s.id}{p.n}"); println("_a6")
    println(f"k{g4(R { id: 13 }, true)}"); println("_a7")
    let r = g5(R { id: 14 }, R { id: 15 }, false); println(f"k{r.id}"); println("_a8")
    let r = g6(Some(R { id: 16 }), Some(R { id: 17 })); println(f"k{r.is_some()}"); println("_a9")
    let b = B { v: 1 }; let r = b.m(R { id: 18 }, R { id: 19 }, false); println(f"k{r.id}"); println("_b1")
    println("end")
}
"#);
    assert_eq!(
        out,
        "d24\nk25\nd25\n_a0\nd23\nk22\nd22\n_b0\nd1\nk2\nd2\n_a1\nd4\nk3\nd3\n_a2\nd5\nk6\nd6\n_a3\nd8\nk7\nd7\n_a4\nd9\nk102\nd10\n_a5\nd12\nk111\nd11\n_a6\nin113\nd113\nd13\nk13\n_a7\ncl\nd14\nk15\nd15\n_a8\nd16\nktrue\nd17\n_a9\nd18\nk19\nd19\n_b1\nend\n",
        "got:\n{out}"
    );
}
