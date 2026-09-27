//! B-2026-09-27-50 — a `let mut` rebind of an owned by-value param that the
//! callee then reassigns runs the param's Drop bodies once.

use super::*;

/// B-2026-09-27-50 — the ASAN twin of
/// `e2e_reassigned_mut_rebind_of_owned_param_runs_body_once`: each owned param
/// rebound by a `let mut` the callee reassigns, and the fresh temp pushed into a
/// callee-local `Vec`, is freed exactly once.
#[test]
fn asan_reassigned_mut_rebind_of_owned_param_is_freed_once() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
enum E { A(R), B }
struct H { k: i64 }
impl H { fn rb(ref self, a: S) -> i64 { let mut c = a; c = mk(90); println("im"); self.k } }
fn ro(a: Option[S]) -> i64 { let mut c = a; c = Some(mk(91)); println("io"); 1 }
fn rr(a: R) -> i64 { let mut c = a; c = R { id: 92 }; println("ir"); 2 }
fn rs(a: S) -> i64 { let mut c = a; println("mid"); c = mk(93); println("is"); 3 }
fn re(a: E) -> i64 { let mut c = a; c = E.A(R { id: 94 }); println("ie"); 4 }
fn rq(a: Result[S, i64]) -> i64 { let mut c = a; c = Ok(mk(95)); println("iq"); 6 }
fn rv(a: Vec[R]) -> i64 { let mut c = a; c = vec![R { id: 96 }]; println("iv"); c.len() }
fn rg[T](a: T, b: T) -> i64 { let mut c = a; c = b; println("ig"); 7 }
fn rp(a: S) -> i64 { let mut v: Vec[S] = Vec.new(); v.push(a); 8 }
fn main() {
    let a: Option[S] = Some(mk(1));
    println(f"k{ro(a)}");
    println(f"k{ro(Some(mk(2)))}");
    let b = R { id: 3 };
    println(f"k{rr(b)}");
    println(f"k{rr(R { id: 4 })}");
    let c = mk(5);
    println(f"k{rs(c)}");
    println(f"k{rs(mk(6))}");
    let d = E.A(R { id: 7 });
    println(f"k{re(d)}");
    println(f"k{re(E.A(R { id: 8 }))}");
    let e: Result[S, i64] = Ok(mk(9));
    println(f"k{rq(e)}");
    println(f"k{rq(Ok(mk(10)))}");
    let f: Vec[R] = vec![R { id: 11 }];
    println(f"k{rv(f)}");
    println(f"k{rv(vec![R { id: 12 }])}");
    let g = mk(13);
    println(f"k{rg(g, mk(14))}");
    println(f"k{rg(mk(15), mk(16))}");
    let h = H { k: 5 };
    let m = mk(17);
    println(f"k{h.rb(m)}");
    println(f"k{h.rb(mk(18))}");
    println(f"k{rp(mk(19))}");
    println("end")
}
"#,
        &[
            "d1", "d91", "io", "k1", "d2", "d91", "io", "k1", "d3", "d92", "ir", "k2", "d4", "d92",
            "ir", "k2", "mid", "d5", "d93", "is", "k3", "mid", "d6", "d93", "is", "k3", "d7",
            "d94", "ie", "k4", "d8", "d94", "ie", "k4", "d9", "d95", "iq", "k6", "d10", "d95",
            "iq", "k6", "d11", "iv", "d96", "k1", "d12", "iv", "d96", "k1", "d13", "ig", "d14",
            "k7", "d15", "ig", "d16", "k7", "d17", "d90", "im", "k5", "d18", "d90", "im", "k5",
            "d19", "k8", "end",
        ],
        "asan_reassigned_mut_rebind_of_owned_param_is_freed_once",
        12,
    );
}
