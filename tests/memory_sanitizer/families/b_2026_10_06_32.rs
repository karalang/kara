//! B-2026-10-06-32 — a bare-expression closure body hands its tail back like a block body does.

use super::*;

/// B-2026-10-06-32 — a closure whose body is a bare expression (`|q: S| q`)
/// hands its value back with the same transfer a block body (`|q: S| { q }`)
/// gets: a `shared` struct or enum param (`a`, `b`, `k`, `n`) gains the
/// caller's reference, and a field projected off one (`m`) is the caller's
/// own copy. Every other shape here (`Option`, `Vec`, `String`, a plain
/// struct, a `Drop` struct, a tuple, a branch tail) keeps its answer. A tail
/// naming a generic boxed-enum param or capture (`o`, `p`, `r`) is handed
/// back as a clone, so the source keeps its cleanup.
///
/// Before: the bare body skipped the tail-return walk, so the result and the
/// argument both released one handle (invalid reads and a double free under
/// valgrind on `a`, `b`, `k`, `m`) while printing the right text; and the
/// block spellings `p` and `r` leaked the per-call copy of the box (24 B).
#[test]
fn asan_identity_closure_hands_back_owned_value() {
    assert_clean_asan_run_min_allocs(
        r#"shared struct S { v: i64, t: String }
shared enum E { A(String), B }
struct H { s: String, k: i64 }
struct R { id: i64 }
enum Ho[T] { Full(T), Empty }
fn mkh(s: String) -> Ho[String] { return Ho.Full(s + "-heap-string-long-enough"); }
fn keep(h: Ho[String]) -> i64 { match h { Ho.Full(s) => s.len(), Ho.Empty => 0 } }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mks(n: i64) -> S { return S { v: n, t: f"t{n}" }; }
fn a() { let s = mks(1); let j = |q: S| q; let u = j(s); println(f"a{u.t}{s.v}") }
fn b() { let e = E.A(f"x{2}"); let j = |q: E| q; let u = j(e); match u { E.A(x) => println(f"b{x}"), E.B => println("bB") } }
fn c() { let o = Some(mks(3)); let j = |q: Option[S]| q; let u = j(o); println(f"c{u.is_some()}") }
fn d() { let v = [1, 2, 3]; let j = |q: Vec[i64]| q; let u = j(v); println(f"d{u.len()}") }
fn e() { let s = f"s{5}"; let j = |q: String| q; let u = j(s); println(f"e{u}") }
fn f() { let h = H { s: f"h{6}", k: 6 }; let j = |q: H| q; let u = j(h); println(f"f{u.s}{u.k}") }
fn g() { let r = R { id: 7 }; let j = |q: R| q; let u = j(r); println(f"g{u.id}") }
fn h() { let p = (mks(8), 8); let j = |q: (S, i64)| q; let u = j(p); let z = u.0; println(f"h{z.v}{u.1}") }
fn k() { let s = mks(9); let j = |q: S| q; let u = j(s); let w = j(u); println(f"k{w.v}") }
fn l() { let s = mks(10); let j = |q: S, n: i64| if n > 0 { q } else { mks(n) }; let u = j(s, 1); println(f"l{u.v}") }
fn m() { let s = mks(11); let j = |q: S| q.t; let u = j(s); println(f"m{u}") }
fn n() { let s = mks(12); let w = s; let j = |q: S| q; println(f"n{j(w).v}") }
fn o() { let f = |q: Ho[String]| q; println(f"o{keep(f(mkh("e")))}") }
fn p() { let f = |q: Ho[String]| { q }; println(f"p{keep(f(mkh("e")))}") }
fn r() { let h = mkh("m"); let f = || { h }; let z = f(); println(f"r{keep(z)}") }
fn main() {
    a()
    b()
    c()
    d()
    e()
    f()
    g()
    h()
    k()
    l()
    m()
    n()
    o()
    p()
    r()
    println("end")
}"#,
        &[
            "at11", "bx2", "ctrue", "d3", "es5", "fh66", "g7", "dR7", "h88", "k9", "l10", "mt11",
            "n12", "o25", "p25", "r25", "end",
        ],
        "asan_identity_closure_hands_back_owned_value",
        8,
    );
}
